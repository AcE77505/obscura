use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use deno_core::error::ModuleLoaderError;
use deno_core::futures::future::{FutureExt, LocalBoxFuture, WeakShared};
use deno_core::ModuleLoadOptions;
use deno_core::ModuleLoadReferrer;
use deno_core::ModuleLoadResponse;
use deno_core::ModuleLoader;
use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleSpecifier;

use deno_error::JsErrorBox;

use crate::import_map::ImportMap;
use crate::ops::{ObscuraState, JsNetworkEvent, StoredNetworkResponseBody, FetchNetworkGuard, record_network_completion};

/// Observable network activity for ES-module graphs.
///
/// deno_core keeps dynamic-import state inside its private module map. The
/// browser lifecycle still needs to distinguish a genuinely idle page from a
/// graph whose fetch future is being advanced in short event-loop slices. A
/// loader-owned counter provides that signal without treating unrelated
/// fetch/XHR analytics as render-blocking work.
#[derive(Debug, Default)]
pub(crate) struct ModuleLoadActivity {
    pending: std::sync::atomic::AtomicUsize,
    last_activity: std::sync::Mutex<Option<std::time::Instant>>,
}

impl ModuleLoadActivity {
    fn begin(self: &Arc<Self>) -> ModuleLoadGuard {
        self.pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(std::time::Instant::now());
        ModuleLoadGuard(self.clone())
    }

    pub(crate) fn is_pending_or_recent(&self, grace: std::time::Duration) -> bool {
        if self.pending.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            return true;
        }
        self.last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some_and(|last| last.elapsed() <= grace)
    }
}

struct ModuleLoadGuard(Arc<ModuleLoadActivity>);

impl Drop for ModuleLoadGuard {
    fn drop(&mut self) {
        let previous = self
            .0
            .pending
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        debug_assert!(previous > 0, "module load activity counter underflow");
        *self
            .0
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(std::time::Instant::now());
    }
}

pub struct ObscuraModuleLoader {
    pub base_url: String,
    /// Proxy URL threaded through to every dynamic ES-module fetch (#139).
    /// `None` keeps the pre-#139 direct-connection behaviour for callers
    /// that haven't been updated.
    pub proxy_url: Option<String>,
    /// The owning page's network context. Production runtimes always install
    /// this so every module in a graph uses the same cookie jar, configured
    /// identity, redirect/security policy, interception, and callbacks as the
    /// entry module. Directly-constructed standalone loaders remain supported.
    page_state: Option<Weak<RefCell<ObscuraState>>>,
    /// Directly-constructed loaders still use Obscura's network policy and
    /// connection pool; they simply have an isolated cookie jar.
    standalone_client: Option<Arc<obscura_net::ObscuraHttpClient>>,
    import_map: Rc<RefCell<ImportMap>>,
    activity: Arc<ModuleLoadActivity>,
    /// Canonical and requested specifiers fetched into deno_core's module map.
    /// The runtime uses a cursor into this append-only list to associate a
    /// prepared root with the dependencies that its successful evaluation also
    /// evaluates.
    loaded_specifiers: Rc<RefCell<Vec<String>>>,
    /// Module fetches that some graph load still awaits, keyed by request URL.
    /// deno_core runs each `import()` and module script as its own graph load
    /// and only dedups a module once it is registered, so concurrent graphs
    /// that share a dependency would each fetch it. A browser module map has a
    /// "fetching" state that makes later requests wait for the first one.
    /// Entries are weak: the waiting loads own the fetch, so a fetch whose
    /// loads were all cancelled is dropped (and reported as failed) as before.
    /// Fetch completion or cancellation removes its own entry without
    /// scanning other pending requests. Failed fetches are not cached.
    inflight: Rc<RefCell<HashMap<String, WeakShared<ModuleFetch>>>>,
}

/// A fetched module body, cheap to clone into each waiting graph load.
#[derive(Clone)]
struct FetchedModule {
    found: ModuleSpecifier,
    code: Arc<str>,
}

type ModuleFetch = LocalBoxFuture<'static, Result<FetchedModule, String>>;

struct InflightFetchGuard {
    inflight: Weak<RefCell<HashMap<String, WeakShared<ModuleFetch>>>>,
    url: String,
}

impl Drop for InflightFetchGuard {
    fn drop(&mut self) {
        if let Some(inflight) = self.inflight.upgrade() {
            inflight.borrow_mut().remove(&self.url);
        }
    }
}

impl ObscuraModuleLoader {
    pub fn new(base_url: &str) -> Self {
        Self::with_proxy(base_url, None)
    }

    pub fn with_proxy(base_url: &str, proxy_url: Option<String>) -> Self {
        let import_map = Rc::new(RefCell::new(ImportMap::default()));
        Self::with_proxy_and_import_map(base_url, proxy_url, import_map)
    }

    fn with_proxy_and_import_map(
        base_url: &str,
        proxy_url: Option<String>,
        import_map: Rc<RefCell<ImportMap>>,
    ) -> Self {
        let standalone_client = Arc::new(obscura_net::ObscuraHttpClient::with_options(
            Arc::new(obscura_net::CookieJar::new()),
            proxy_url.as_deref(),
        ));
        ObscuraModuleLoader {
            base_url: base_url.to_string(),
            proxy_url,
            page_state: None,
            standalone_client: Some(standalone_client),
            import_map,
            activity: Arc::new(ModuleLoadActivity::default()),
            loaded_specifiers: Rc::new(RefCell::new(Vec::new())),
            inflight: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    pub(crate) fn with_page_state(
        base_url: &str,
        proxy_url: Option<String>,
        page_state: &Rc<RefCell<ObscuraState>>,
        import_map: Rc<RefCell<ImportMap>>,
    ) -> Self {
        ObscuraModuleLoader {
            base_url: base_url.to_string(),
            proxy_url,
            page_state: Some(Rc::downgrade(page_state)),
            standalone_client: None,
            import_map,
            activity: Arc::new(ModuleLoadActivity::default()),
            loaded_specifiers: Rc::new(RefCell::new(Vec::new())),
            inflight: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    pub(crate) fn activity(&self) -> Arc<ModuleLoadActivity> {
        self.activity.clone()
    }

    pub(crate) fn loaded_specifiers(&self) -> Rc<RefCell<Vec<String>>> {
        self.loaded_specifiers.clone()
    }

    /// The page's network context for a new module fetch.
    fn page_network(&self) -> PageNetwork {
        match self.page_state.as_ref() {
            Some(weak) => (|| {
                let state = weak
                    .upgrade()
                    .ok_or_else(|| "Module loader page state was dropped".to_string())?;
                let state = state
                    .try_borrow()
                    .map_err(|_| "Module loader page state is already borrowed".to_string())?;
                let client = state
                    .http_client
                    .clone()
                    .ok_or_else(|| "No http_client wired to module loader".to_string())?;
                // Fork: in stealth mode an ES module must be fetched over the
                // same transport as the document. Upstream sends it through the
                // plain reqwest client, so a `type="module"` script arrives with
                // a different TLS fingerprint and none of the browser identity
                // headers, while the HTML that referenced it came over wreq.
                // That cross-transport mismatch is trivially detectable.
                #[cfg(feature = "stealth")]
                let stealth = state.stealth_client.clone();
                #[cfg(not(feature = "stealth"))]
                let stealth: Option<std::sync::Arc<()>> = None;
                Ok((client, stealth, state.callbacks.clone()))
            })(),
            None => self
                .standalone_client
                .clone()
                .map(|client| {
                    #[cfg(feature = "stealth")]
                    let stealth = None;
                    #[cfg(not(feature = "stealth"))]
                    let stealth: Option<std::sync::Arc<()>> = None;
                    (client, stealth, None)
                })
                .ok_or_else(|| "No network context wired to module loader".to_string()),
        }
    }
}

fn io_err(msg: String) -> ModuleLoaderError {
    JsErrorBox::from_err(std::io::Error::new(std::io::ErrorKind::Other, msg))
}

impl ModuleLoader for ObscuraModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: deno_core::ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        // deno_core represents the root passed to load_side_es_module with a
        // synthetic "." referrer. A browser resolves <script type=module src>
        // as a resource URL before it starts a graph; the document import map
        // must not remap that root URL.
        if referrer == "." {
            return deno_core::resolve_import(specifier, &self.base_url)
                .map_err(|error| io_err(error.to_string()));
        }

        let base = if referrer.is_empty()
            || referrer.starts_with('<')
            || referrer == "about:blank"
        {
            &self.base_url
        } else {
            referrer
        };

        let base = ModuleSpecifier::parse(base)
            .map_err(|e| io_err(format!("Invalid module referrer {}: {}", base, e)))?;
        self.import_map
            .try_borrow_mut()
            .map_err(|_| io_err("Import map is already borrowed".to_string()))?
            .resolve(specifier, &base)
            .map_err(io_err)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        maybe_referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let url = module_specifier.to_string();
        // Module-graph CORS and same-origin credentials are relative to the
        // owning document, not to the importing module. The importer remains
        // the HTTP referrer for a dependency; keeping these URLs distinct
        // prevents a cross-origin parent module from gaining CDN cookies when
        // it imports a sibling on that CDN.
        let document_url = ModuleSpecifier::parse(&self.base_url)
            .unwrap_or_else(|_| module_specifier.clone());
        let referrer = maybe_referrer
            .map(|referrer| referrer.specifier.clone())
            .unwrap_or_else(|| document_url.clone());
        // Capture the loader's proxy here so the async closure below owns a
        // plain Option<String> rather than borrowing &self across an `await`.
        let proxy_url = self.proxy_url.clone();
        let activity = self.activity.clone();
        let loaded_specifiers = self.loaded_specifiers.clone();
        loaded_specifiers.borrow_mut().push(url.clone());
        // Register before returning the future. The lifecycle can inspect the
        // runtime between deno_core accepting the load and first polling it.
        // Keeping the guard inside the future makes cancellation/navigation
        // decrement the count through Drop as well as success and failure.
        let is_dyn_import = options.is_dynamic_import;
        let activity_guard = is_dyn_import.then(|| activity.begin());
        let page_state = self.page_state.clone();

        let requested_specifier = module_specifier.clone();
        let fetch = {
            let mut inflight = self.inflight.borrow_mut();
            match inflight.get(&url).and_then(WeakShared::upgrade) {
                Some(fetch) => fetch,
                None => {
                    let page_network = self.page_network();
                    let cleanup = InflightFetchGuard {
                        inflight: Rc::downgrade(&self.inflight),
                        url: url.clone(),
                    };
                    let fetch_url = url.clone();
                    let fetch = async move {
                        let _cleanup = cleanup;
                        fetch_module(fetch_url, proxy_url, document_url, referrer,
                            page_state, page_network).await.map_err(|error| error.to_string())
                    }
                    .boxed_local()
                    .shared();
                    if let Some(weak) = fetch.downgrade() {
                        inflight.insert(url, weak);
                    }
                    fetch
                }
            }
        };

        ModuleLoadResponse::Async(Pin::from(Box::new(async move {
            // deno_core propagates `is_dynamic_import` to every dependency edge in
            // the recursive graph, so this excludes parser-discovered/static
            // graphs without losing descendant fetches of a lazy graph.
            let _activity_guard = activity_guard;
            let fetched = fetch.await.map_err(io_err)?;
            if fetched.found.as_str() != requested_specifier.as_str() {
                loaded_specifiers.borrow_mut().push(fetched.found.to_string());
            }
            Ok(ModuleSource::new_with_redirect(
                deno_core::ModuleType::JavaScript,
                ModuleSourceCode::String(fetched.code.into()),
                &requested_specifier,
                &fetched.found,
                None,
            ))
        })))
    }
}

#[cfg(feature = "stealth")]
type StealthClient = Option<Arc<obscura_net::StealthHttpClient>>;
#[cfg(not(feature = "stealth"))]
type StealthClient = Option<Arc<()>>;

type PageNetwork = Result<
    (
        Arc<obscura_net::ObscuraHttpClient>,
        StealthClient,
        Option<Arc<obscura_net::CallbackRegistry>>,
    ),
    String,
>;

async fn fetch_module(
    url: String,
    proxy_url: Option<String>,
    document_url: ModuleSpecifier,
    referrer: ModuleSpecifier,
    page_state: Option<Weak<RefCell<ObscuraState>>>,
    page_network: PageNetwork,
) -> Result<FetchedModule, ModuleLoaderError> {
    tracing::debug!(
        "Loading ES module: {} (proxy: {})",
        url,
        proxy_url.as_deref().unwrap_or("direct")
    );

    match page_network {
        Ok((client, stealth, callbacks)) => {
            let requested = ModuleSpecifier::parse(&url)
                .map_err(|e| io_err(format!("Invalid module URL {}: {}", url, e)))?;
            static NEXT_MODULE_REQUEST_ID: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(1);
            let request_id = format!("module-{}", NEXT_MODULE_REQUEST_ID.fetch_add(
                1, std::sync::atomic::Ordering::Relaxed,
            ));
            // Keep failed and cancelled graph fetches visible to the
            // same Network listeners as ordinary fetch/XHR failures.
            let mut network_guard = page_state.as_ref().map(|state| FetchNetworkGuard {
                state: state.clone(), cancel: None,
                event: Some(JsNetworkEvent {
                    request_id: request_id.clone(), intercepted: false,
                    url: url.clone(), method: "GET".to_string(),
                    resource_type: "Script".to_string(), status: 0,
                    response_headers: Default::default(), body_size: 0,
                    timestamp: 0.0, error_text: None,
                }),
            });
            let request =
                obscura_net::ResourceRequest::module_script(&document_url, &referrer);
            #[cfg(feature = "stealth")]
            let resp = match stealth {
                Some(stealth) => stealth
                    .fetch_resource_with_callbacks(
                        &requested,
                        request,
                        callbacks.as_deref(),
                    )
                    .await,
                None => {
                    client
                        .fetch_resource_with_callbacks(
                            &requested,
                            request,
                            callbacks.as_deref(),
                        )
                        .await
                }
            }
            .map_err(|e| io_err(format!("Failed to fetch module {}: {}", url, e)))?;
            #[cfg(not(feature = "stealth"))]
            let resp = {
                let _ = &stealth;
                client
                    .fetch_resource_with_callbacks(
                        &requested,
                        request,
                        callbacks.as_deref(),
                    )
                    .await
                    .map_err(|e| io_err(format!("Failed to fetch module {}: {}", url, e)))?
            };
            // A response (including the blocked-tracker sentinel) is
            // not a transport failure, even if evaluation later fails.
            if let Some(guard) = &mut network_guard { guard.event = None; }
            // A module response belongs to the owning page regardless
            // of whether graph preparation or evaluation later fails.
            // Status 0 is the transport's blocked-tracker sentinel.
            if resp.status != 0 {
                if let Some(state) = page_state.as_ref().and_then(Weak::upgrade) {
                    let mut state = state.try_borrow_mut().map_err(|_| {
                        io_err("Module loader page state is already borrowed".to_string())
                    })?;
                    record_network_completion(&mut state, JsNetworkEvent {
                        request_id, intercepted: false,
                        url: resp.url.to_string(), method: "GET".to_string(),
                        resource_type: "Script".to_string(), status: resp.status,
                        response_headers: resp.headers.clone(), body_size: resp.body.len(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default().as_secs_f64(),
                        error_text: None,
                    }, || match std::str::from_utf8(&resp.body) {
                        Ok(body) => StoredNetworkResponseBody {
                            body: body.to_string(), base64_encoded: false,
                        },
                        Err(_) => StoredNetworkResponseBody {
                            body: BASE64.encode(&resp.body), base64_encoded: true,
                        },
                    });
                }
            }
            if !(200..=299).contains(&resp.status) {
                return Err(io_err(format!(
                    "Module {} returned HTTP {}",
                    url, resp.status
                )));
            }
            let found = ModuleSpecifier::parse(resp.url.as_str()).map_err(|e| {
                io_err(format!("Invalid final module URL {}: {}", resp.url, e))
            })?;
            let code = obscura_net::decode_non_html(&resp.body, resp.content_type());
            Ok(FetchedModule { found, code: Arc::from(code) })
        }
        Err(error) => Err(io_err(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_load(loader: &ObscuraModuleLoader, url: &str) -> ModuleLoadResponse {
        loader.load(&ModuleSpecifier::parse(url).unwrap(), None, ModuleLoadOptions {
            is_dynamic_import: true,
            is_synchronous: false,
            requested_module_type: deno_core::RequestedModuleType::None,
        })
    }

    #[test]
    fn cancelled_module_loads_release_inflight_entries_without_another_request() {
        let loader = ObscuraModuleLoader::new("https://example.com/");
        let first = pending_load(&loader, "https://example.com/shared.js");
        let second = pending_load(&loader, "https://example.com/shared.js");
        assert_eq!(loader.inflight.borrow().len(), 1);
        drop(first);
        assert_eq!(loader.inflight.borrow().len(), 1, "another graph still owns the fetch");
        drop(second);
        assert!(loader.inflight.borrow().is_empty(), "cancelled URLs must not remain in the map");
        assert!(!loader.activity.is_pending_or_recent(std::time::Duration::ZERO));
    }

    #[test]
    fn cancelling_large_module_fanout_clears_all_entries() {
        for count in [1_000, 4_000, 8_000] {
            let loader = ObscuraModuleLoader::new("https://example.com/");
            let start = std::time::Instant::now();
            let loads: Vec<_> = (0..count).map(|index| {
                pending_load(&loader, &format!("https://example.com/{index}.js"))
            }).collect();
            eprintln!("{count} pending module loads: {:?}", start.elapsed());
            assert_eq!(loader.inflight.borrow().len(), count);
            drop(loads);
            assert!(loader.inflight.borrow().is_empty());
        }
    }

    fn network_loader(base: &str) -> ObscuraModuleLoader {
        let mut loader = ObscuraModuleLoader::new(base);
        loader.standalone_client = Some(Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            Arc::new(obscura_net::CookieJar::new()), None, true,
        )));
        loader
    }

    fn async_load(loader: &ObscuraModuleLoader, url: &str) -> Pin<Box<deno_core::ModuleSourceFuture>> {
        match pending_load(loader, url) {
            ModuleLoadResponse::Async(load) => load,
            ModuleLoadResponse::Sync(_) => panic!("network loads must be asynchronous"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_failed_module_fetch_can_be_retried() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/shared.js", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for status in ["503 Service Unavailable", "200 OK"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                stream.read(&mut request).await.unwrap();
                let body = "export const value = 42;";
                stream.write_all(format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                ).as_bytes()).await.unwrap();
            }
        });
        let loader = network_loader(&url);
        let (first, second) = tokio::join!(async_load(&loader, &url), async_load(&loader, &url));
        for result in [first, second] {
            let Err(error) = result else { panic!("HTTP failure must reach every waiter") };
            assert!(error.to_string().contains("HTTP 503"), "{error}");
        }
        assert!(loader.inflight.borrow().is_empty());
        assert!(async_load(&loader, &url).await.is_ok(), "a failure must not poison future loads");
        assert!(loader.inflight.borrow().is_empty());
        tokio::time::timeout(std::time::Duration::from_secs(2), server).await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_one_active_module_waiter_keeps_the_shared_fetch() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/shared.js", listener.local_addr().unwrap());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            stream.read(&mut request).await.unwrap();
            started_tx.send(()).unwrap();
            release_rx.await.unwrap();
            let body = "export const value = 42;";
            stream.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            ).as_bytes()).await.unwrap();
        });
        let loader = network_loader(&url);
        let mut first = async_load(&loader, &url);
        let second = async_load(&loader, &url);
        tokio::select! {
            result = &mut first => panic!("held response completed: {}", result.is_ok()),
            result = started_rx => result.unwrap(),
        }
        drop(first);
        assert_eq!(loader.inflight.borrow().len(), 1);
        release_tx.send(()).unwrap();
        assert!(tokio::time::timeout(std::time::Duration::from_secs(2), second).await.unwrap().is_ok());
        assert!(loader.inflight.borrow().is_empty());
        server.await.unwrap();
    }
}
