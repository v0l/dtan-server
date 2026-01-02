use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONNECTION, SEC_WEBSOCKET_ACCEPT, UPGRADE};
use hyper::service::Service;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use log::{error, info};
use nostr_relay_builder::LocalRelay;
use nostr_relay_builder::prelude::RelayInformationDocument;
use nostr_sdk::JsonUtil;
use sha1::{Digest, Sha1};
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;

pub struct HttpServer {
    relay: LocalRelay,
    remote: SocketAddr,
    ui_dir: PathBuf,
    relay_document: RelayInformationDocument,
}

impl HttpServer {
    pub fn new(
        relay: LocalRelay,
        remote: SocketAddr,
        ui_dir: PathBuf,
        doc: RelayInformationDocument,
    ) -> Self {
        HttpServer {
            relay,
            remote,
            ui_dir,
            relay_document: doc,
        }
    }
}

fn derive_accept_key(request_key: &[u8]) -> String {
    const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let mut engine = Sha1::default();
    engine.update(request_key);
    engine.update(WS_GUID);
    BASE64_STANDARD.encode(engine.finalize())
}

impl Service<Request<Incoming>> for HttpServer {
    type Response = Response<http_body_util::Full<Bytes>>;
    type Error = anyhow::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let base = Response::builder().header("server", "DTAN").status(404);

        // check is upgrade
        if let (Some(c), Some(w)) = (
            req.headers().get("connection"),
            req.headers().get("upgrade"),
        ) {
            if c.to_str()
                .map(|s| s.to_lowercase() == "upgrade")
                .unwrap_or(false)
                && w.to_str()
                    .map(|s| s.to_lowercase() == "websocket")
                    .unwrap_or(false)
            {
                let key = req.headers().get("sec-websocket-key");
                let derived = key.map(|k| derive_accept_key(k.as_bytes()));

                let addr = self.remote.clone();
                let relay = self.relay.clone();
                tokio::spawn(async move {
                    match hyper::upgrade::on(req).await {
                        Ok(upgraded) => {
                            if let Err(e) =
                                relay.take_connection(TokioIo::new(upgraded), addr).await
                            {
                                error!("{}", e);
                            }
                        }
                        Err(e) => error!("{}", e),
                    }
                });
                return Box::pin(async move {
                    base.status(101)
                        .header(CONNECTION, "upgrade")
                        .header(UPGRADE, "websocket")
                        .header(SEC_WEBSOCKET_ACCEPT, derived.unwrap())
                        .body(http_body_util::Full::new(Bytes::new()))
                        .map_err(anyhow::Error::from)
                });
            }
        }

        // map request to ui dir
        let web_path = req.uri().path();
        let is_root = web_path.is_empty() || web_path == "/";

        const CT_RELAY_DOC: &str = "application/nostr+json";

        // serve relay document
        if is_root
            && let Some(ac) = req.headers().get("accept")
            && ac == CT_RELAY_DOC
        {
            let mut doc = self.relay_document.clone();
            doc.supported_nips = Some(vec![1, 50, 77]);
            doc.software = Some("git+https://github.com/v0l/dtan-server".to_string());
            doc.version = Some("0.1.0".to_string());
            doc.icon = Some("https://dtan.xyz/logo.png".to_string());

            let json = doc.as_json();
            return Box::pin(async move {
                base.status(200)
                    .header("content-type", CT_RELAY_DOC)
                    .body(http_body_util::Full::new(Bytes::from_owner(
                        json.into_bytes(),
                    )))
                    .map_err(anyhow::Error::from)
            });
        }

        let dst = if is_root {
            self.ui_dir.join("index.html")
        } else {
            let new_path = self.ui_dir.join(&web_path[1..]);
            if new_path.exists() {
                new_path
            } else {
                self.ui_dir.join("index.html")
            }
        };
        if dst.exists() {
            let remote_addr = self.remote.clone();
            return Box::pin(async move {
                if let Ok(data) = tokio::fs::read(&dst).await {
                    let ct = match dst.extension().and_then(|s| s.to_str()) {
                        Some("css") => "text/css",
                        Some("html") => "text/html",
                        Some("js") => "text/javascript",
                        Some("mjs") => "text/javascript",
                        Some("jpg") => "image/jpeg",
                        Some("png") => "image/png",
                        Some("svg") => "image/svg+xml",
                        Some("json") => "application/json",
                        Some(".wasm") => "application/wasm",
                        Some("woff2") => "application/woff",
                        _ => "application/octet-stream",
                    };
                    info!(
                        "[{}] {} 200 {} {}",
                        remote_addr,
                        dst.display(),
                        data.len(),
                        ct
                    );
                    base.status(200)
                        .header("content-type", ct)
                        .body(http_body_util::Full::new(Bytes::from_owner(data)))
                        .map_err(anyhow::Error::from)
                } else {
                    base.body(http_body_util::Full::new(Bytes::new()))
                        .map_err(anyhow::Error::from)
                }
            });
        }
        // serve landing page otherwise
        Box::pin(async move {
            base.body(http_body_util::Full::new(Bytes::new()))
                .map_err(anyhow::Error::from)
        })
    }
}
