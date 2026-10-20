use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lsp_types::request::Request;
use reflexo::ImmutPath;
use reflexo_typst::vfs::{FileChangeSet, PathAccessModel};
use reflexo_typst::Bytes;
use serde::{Deserialize, Serialize};
use sync_ls::TypedLspClient;
use tinymist_project::{Interrupt, FILE_MISSING_ERROR};
use typst::diag::FileResult;

use crate::vfs::notify::NotifyMessage;
use crate::ServerState;

impl ServerState {
    /// Handles the dependency changes.
    pub(crate) fn handle_deps(&mut self) {
        let Some(dep_rx) = self.dep_rx.as_mut() else {
            return;
        };

        while let Ok(msg) = dep_rx.try_recv() {
            match msg {
                NotifyMessage::Settle => {}
                NotifyMessage::SyncDependency(notify_deps) => {
                    let mut deps = HashSet::new();
                    // todo: poor performance
                    notify_deps.dependencies(&mut |path| {
                        deps.insert(path.clone());
                    });

                    let am = self.config.watch_access_model(&self.client);
                    am.retain_watch(|path| deps.contains(path));
                }
                NotifyMessage::UpstreamUpdate(evt) => {
                    let changeset = FileChangeSet::new_inserts(vec![]);
                    self.project.interrupt(Interrupt::Fs(
                        crate::vfs::FilesystemEvent::UpstreamUpdate {
                            changeset,
                            upstream_event: Some(evt),
                        },
                    ));
                }
            }
        }
    }
}

/// The access model that watches the file content from the client.
#[derive(Clone)]
pub struct WatchAccessModel {
    pub watches: Arc<Mutex<HashSet<ImmutPath>>>,
    pub client: TypedLspClient<ServerState>,
}

impl WatchAccessModel {
    pub fn new(client: TypedLspClient<ServerState>) -> Self {
        Self {
            client,
            watches: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn add_watch(&self, path: &Path) {
        let path = ImmutPath::from(path);
        // The compilation may repeatedly query a file that is not available
        // locally. Ask the client only once per file to avoid a busy loop of
        // `tinymist/fs/watch` requests.
        if self.watches.lock().unwrap().contains(&path) {
            return;
        }

        log::info!("add watch: {path:?}");
        let req = FsWatchRequest {
            inserts: vec![tinymist_query::path_to_url(path.as_ref()).unwrap()],
            removes: vec![],
        };
        self.client
            .send_lsp_request::<FsWatchRequest>(req, |_stat, resp| {
                if let Some(e) = resp.result {
                    log::error!("Failed to watch file: {e:?}");
                }
            });

        let mut watches = self.watches.lock().unwrap();
        watches.insert(path);
    }

    pub fn retain_watch(&self, filter: impl Fn(&Path) -> bool) {
        let mut removes = vec![];

        let mut watches = self.watches.lock().unwrap();
        watches.retain(|path| {
            if !filter(path) {
                // todo: clone here
                removes.push(tinymist_query::path_to_url(path.as_ref()).unwrap());
                return false;
            }

            true
        });
        drop(watches);

        let req = FsWatchRequest {
            inserts: vec![],
            removes,
        };
        self.client
            .send_lsp_request::<FsWatchRequest>(req, |_stat, resp| {
                if let Some(e) = resp.result {
                    log::error!("Failed to watch file: {e:?}");
                }
            });
    }
}

impl PathAccessModel for WatchAccessModel {
    fn content(&self, src: &Path) -> FileResult<Bytes> {
        // Requests to load the file content from the client.
        //
        // Note that we intentionally do not block here until the client
        // delivers the content: the compilation runs on the same thread that
        // processes incoming LSP messages, so waiting would prevent the
        // `tinymist/fsChange` request from ever being handled. Instead, the
        // client pushes the content back, which interrupts and re-schedules the
        // compilation with the file available in the in-memory file system.
        self.add_watch(src);
        // Returns an error to indicate that the file is not available locally.
        Err(FILE_MISSING_ERROR.clone())
    }
}

/// The access model that reads files from the local file system and falls back
/// to the client for the files that are not available locally.
///
/// This is used when the language server runs on a remote host. The server can
/// still read its own resources, such as the Typst packages installed on the
/// server, while the files of the workspace are streamed from the client on
/// demand.
#[cfg(feature = "system")]
pub struct FallbackAccessModel {
    /// The access model reading the local file system.
    pub local: reflexo_typst::vfs::system::SystemAccessModel,
    /// The access model asking the client for file contents.
    pub remote: Arc<WatchAccessModel>,
}

#[cfg(feature = "system")]
impl PathAccessModel for FallbackAccessModel {
    fn content(&self, src: &Path) -> FileResult<Bytes> {
        match self.local.content(src) {
            Ok(content) => Ok(content),
            // Requests the content from the client if the file does not exist
            // locally.
            Err(err) => {
                self.remote.add_watch(src);
                Err(err)
            }
        }
    }
}

/// The file system watch request for the client.
/// This is used to watch the file content from the client.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsWatchRequest {
    inserts: Vec<lsp_types::Url>,
    removes: Vec<lsp_types::Url>,
}

impl Request for FsWatchRequest {
    type Params = Self;
    type Result = ();
    const METHOD: &'static str = "tinymist/fs/watch";
}
