//! `WKDownloadDelegate` and the unique-destination resolver. The
//! delegate emits `NavigationEvent::DownloadStarted` /
//! `DownloadFinished` onto the producer's nav-event FIFO; one
//! instance is shared across every `WKDownload` we receive (WebKit
//! holds delegate references weakly via `WKDownload::setDelegate:`,
//! so the strong ref lives on the producer).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_foundation::{
    MainThreadMarker, NSData, NSError, NSObject, NSObjectProtocol, NSString, NSURLResponse, NSURL,
};
use objc2_web_kit::{WKDownload, WKDownloadDelegate};

use crate::NavigationEvent;

use super::nav_delegate::NavState;

/// `DownloadHandler`'s ivars: the shared nav-event FIFO it pushes
/// into and the directory where it writes downloaded files.
pub(super) struct DownloadHandlerIvars {
    pub(super) state: Arc<Mutex<NavState>>,
    pub(super) download_dir: PathBuf,
}

// `WKDownloadDelegate` implementation. Chooses a destination path
// under the configured download dir, emits `DownloadStarted` on the
// nav-event FIFO when destination resolution completes, emits
// `DownloadFinished` on success or failure. One instance is shared
// across all downloads from a given producer; WebKit holds its
// reference weakly via `WKDownload::setDelegate:`, so the producer
// keeps the strong ref alive.
define_class!(
    // SAFETY:
    // - Superclass NSObject has no subclassing requirements.
    // - `DownloadHandler` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = DownloadHandlerIvars]
    pub(super) struct DownloadHandler;

    unsafe impl NSObjectProtocol for DownloadHandler {}

    unsafe impl WKDownloadDelegate for DownloadHandler {
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        fn decide_destination(
            &self,
            _download: &WKDownload,
            response: &NSURLResponse,
            suggested_filename: &NSString,
            completion_handler: &block2::DynBlock<dyn Fn(*mut NSURL)>,
        ) {
            let suggested = suggested_filename.to_string();
            let url = response
                .URL()
                .and_then(|u| u.absoluteString())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let ivars = self.ivars();

            // Ensure download_dir exists. If creation fails the
            // downstream NSURL conversion will surface the error
            // via `download:didFailWithError:`.
            let _ = std::fs::create_dir_all(&ivars.download_dir);

            // Resolve path collisions by appending `-N` before the
            // extension, then `-N+1`, etc.
            let destination = unique_destination(&ivars.download_dir, &suggested);
            let destination_str = destination.to_string_lossy().into_owned();
            let path_ns = NSString::from_str(&destination_str);
            let file_url = NSURL::fileURLWithPath(&path_ns);

            if let Ok(mut state) = ivars.state.lock() {
                state.events.push_back(NavigationEvent::DownloadStarted {
                    url,
                    suggested_filename: suggested,
                    destination_path: destination,
                });
            }
            // Hand the chosen URL back to WebKit. Apple's docs
            // require the handler to be invoked exactly once.
            completion_handler.call((Retained::as_ptr(&file_url) as *mut _,));
        }

        #[unsafe(method(downloadDidFinish:))]
        fn did_finish(&self, _download: &WKDownload) {
            // We don't track per-download state mapping
            // download→destination_path here, so we surface
            // completion without a path. Callers that need
            // per-download bookkeeping can correlate via the
            // matching DownloadStarted event order.
            if let Ok(mut state) = self.ivars().state.lock() {
                state.events.push_back(NavigationEvent::DownloadFinished {
                    destination_path: PathBuf::new(),
                    error: None,
                });
            }
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn did_fail(
            &self,
            _download: &WKDownload,
            error: &NSError,
            _resume_data: Option<&NSData>,
        ) {
            let msg = error.localizedDescription().to_string();
            if let Ok(mut state) = self.ivars().state.lock() {
                state.events.push_back(NavigationEvent::DownloadFinished {
                    destination_path: PathBuf::new(),
                    error: Some(msg),
                });
            }
        }
    }
);

impl DownloadHandler {
    pub(super) fn new(
        mtm: MainThreadMarker,
        state: Arc<Mutex<NavState>>,
        download_dir: PathBuf,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DownloadHandlerIvars {
            state,
            download_dir,
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// Pick a path under `dir` that doesn't already exist. If
/// `<dir>/<name>` exists, try `<dir>/<stem>-1.<ext>`,
/// `<dir>/<stem>-2.<ext>`, etc. Caller is responsible for
/// `create_dir_all(dir)`.
fn unique_destination(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let stem;
    let ext;
    if let Some(dot) = name.rfind('.') {
        stem = &name[..dot];
        ext = &name[dot..]; // includes the leading "."
    } else {
        stem = name;
        ext = "";
    }
    for n in 1..u32::MAX {
        let candidate = dir.join(format!("{stem}-{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    // Astronomically unlikely; fall back to the original name.
    dir.join(name)
}
