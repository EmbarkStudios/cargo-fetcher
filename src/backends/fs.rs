use crate::{CloudId, PathBuf};
use anyhow::Result;
use bytes::Bytes;
use std::fs;

#[derive(Debug)]
pub struct FsBackend {
    path: PathBuf,
}

impl FsBackend {
    pub fn new(loc: crate::FilesystemLocation<'_>) -> Result<Self> {
        let crate::FilesystemLocation { path } = loc;

        if !path.exists() {
            fs::create_dir_all(path)?;
        }

        Ok(Self {
            path: path.to_owned(),
        })
    }

    #[inline]
    fn make_path(&self, id: CloudId<'_>) -> PathBuf {
        self.path.join(id.to_string())
    }
}

#[async_trait::async_trait]
impl crate::Backend for FsBackend {
    async fn fetch(&self, id: CloudId<'_>) -> Result<Bytes> {
        let path = self.make_path(id);
        let buf = fs::read(path)?;
        Ok(buf.into())
    }

    async fn upload(&self, source: Bytes, id: CloudId<'_>) -> Result<usize> {
        let path = self.make_path(id);
        fs::write(path, &source)?;
        Ok(source.len())
    }

    async fn list(&self) -> Result<Vec<String>> {
        let entries = fs::read_dir(&self.path)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                entry.file_type().ok().filter(|ft| ft.is_file())?;
                entry.file_name().into_string().ok()
            })
            .collect();

        Ok(entries)
    }

    async fn updated(&self, id: CloudId<'_>) -> Result<Option<crate::Timestamp>> {
        let path = self.make_path(id);

        if !path.exists() {
            return Ok(None);
        }

        let metadata = fs::metadata(&path)?;
        let modified = metadata.modified()?.into();

        Ok(Some(modified))
    }
}
