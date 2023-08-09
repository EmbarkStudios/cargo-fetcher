#[cfg(feature = "gcs")]
pub mod gcs;

#[cfg(feature = "s3")]
pub mod s3;

pub mod fs;

#[cfg(feature = "blob")]
pub mod blob;
