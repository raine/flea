use std::{io, path::Path};

pub trait AtomicFileStore {
    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()>;
}
