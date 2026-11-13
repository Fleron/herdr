use super::*;

impl FileTransport {
    #[cfg(all(test, unix))]
    pub(crate) fn for_test(root: PathBuf) -> Self {
        Self {
            inner: Some(crate::platform::unix_image_files::Ledger::for_test(root)),
        }
    }
}
