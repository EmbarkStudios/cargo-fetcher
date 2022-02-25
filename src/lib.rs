// BEGIN - Embark standard lints v5 for Rust 1.55+
// do not change or add/remove here, but one can add exceptions after this section
// for more info see: <https://github.com/EmbarkStudios/rust-ecosystem/issues/59>
#![deny(unsafe_code)]
#![warn(
    clippy::all,
    clippy::await_holding_lock,
    clippy::char_lit_as_u8,
    clippy::checked_conversions,
    clippy::dbg_macro,
    clippy::debug_assert_with_mut_call,
    clippy::doc_markdown,
    clippy::empty_enum,
    clippy::enum_glob_use,
    clippy::exit,
    clippy::expl_impl_clone_on_copy,
    clippy::explicit_deref_methods,
    clippy::explicit_into_iter_loop,
    clippy::fallible_impl_from,
    clippy::filter_map_next,
    clippy::flat_map_option,
    clippy::float_cmp_const,
    clippy::fn_params_excessive_bools,
    clippy::from_iter_instead_of_collect,
    clippy::if_let_mutex,
    clippy::implicit_clone,
    clippy::imprecise_flops,
    clippy::inefficient_to_string,
    clippy::invalid_upcast_comparisons,
    clippy::large_digit_groups,
    clippy::large_stack_arrays,
    clippy::large_types_passed_by_value,
    clippy::let_unit_value,
    clippy::linkedlist,
    clippy::lossy_float_literal,
    clippy::macro_use_imports,
    clippy::manual_ok_or,
    clippy::map_err_ignore,
    clippy::map_flatten,
    clippy::map_unwrap_or,
    clippy::match_on_vec_items,
    clippy::match_same_arms,
    clippy::match_wild_err_arm,
    clippy::match_wildcard_for_single_variants,
    clippy::mem_forget,
    clippy::mismatched_target_os,
    clippy::missing_enforced_import_renames,
    clippy::mut_mut,
    clippy::mutex_integer,
    clippy::needless_borrow,
    clippy::needless_continue,
    clippy::needless_for_each,
    clippy::option_option,
    clippy::path_buf_push_overwrite,
    clippy::ptr_as_ptr,
    clippy::rc_mutex,
    clippy::ref_option_ref,
    clippy::rest_pat_in_fully_bound_structs,
    clippy::same_functions_in_if_condition,
    clippy::semicolon_if_nothing_returned,
    clippy::single_match_else,
    clippy::string_add_assign,
    clippy::string_add,
    clippy::string_lit_as_bytes,
    clippy::string_to_string,
    clippy::todo,
    clippy::trait_duplication_in_bounds,
    clippy::unimplemented,
    clippy::unnested_or_patterns,
    clippy::unused_self,
    clippy::useless_transmute,
    clippy::verbose_file_reads,
    clippy::zero_sized_map_values,
    future_incompatible,
    nonstandard_style,
    rust_2018_idioms
)]
// END - Embark standard lints v0.5 for Rust 1.55+
// crate-specific exceptions:
#![allow(clippy::single_match_else)]

use anyhow::Error;
use std::{
    convert::From,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tracing::warn;
pub use url::Url;

pub mod backends;
pub mod cargo;
mod fetch;
pub(crate) mod git;
pub mod mirror;
pub mod sync;
pub mod util;

pub type HttpClient = reqwest::blocking::Client;

pub use cargo::{read_cargo_config, Registry, Source};

#[derive(Eq, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Krate {
    pub name: String,
    pub version: String, // We just treat versions as opaque strings
    pub source: Source,
}

// impl tracing::Value for Krate {
//     fn record(&self, key: &tracing::field::Field, visitor: &mut dyn tracing::field::Visit) {
//         visitor.record_debug(key, self)
//     }
// }

impl Ord for Krate {
    fn cmp(&self, b: &Self) -> std::cmp::Ordering {
        self.source.cmp(&b.source)
    }
}

impl PartialOrd for Krate {
    fn partial_cmp(&self, b: &Self) -> Option<std::cmp::Ordering> {
        self.source.partial_cmp(&b.source)
    }
}

impl PartialEq for Krate {
    fn eq(&self, b: &Self) -> bool {
        self.source.eq(&b.source)
    }
}

impl PartialEq<Registry> for Krate {
    fn eq(&self, b: &Registry) -> bool {
        match &self.source {
            Source::Git { .. } => false,
            Source::Registry { registry, .. } => b.eq(registry),
        }
    }
}

impl Krate {
    pub fn cloud_id(&self) -> CloudId<'_> {
        CloudId { inner: self }
    }

    pub fn local_id(&self) -> LocalId<'_> {
        LocalId { inner: self }
    }
}

impl fmt::Display for Krate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let typ = match &self.source {
            Source::Git { .. } => "git",
            Source::Registry { .. } => "registry",
        };

        write!(f, "{}-{}({})", self.name, self.version, typ)
    }
}

pub struct LocalId<'a> {
    inner: &'a Krate,
}

impl<'a> fmt::Display for LocalId<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner.source {
            Source::Git { ident, .. } => write!(f, "{}", &ident),
            Source::Registry { .. } => {
                write!(f, "{}-{}.crate", self.inner.name, self.inner.version)
            }
        }
    }
}

pub struct CloudId<'a> {
    inner: &'a Krate,
}

impl<'a> fmt::Display for CloudId<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner.source {
            Source::Git { ident, rev, .. } => write!(f, "{}-{}", ident, rev),
            Source::Registry { chksum, .. } => write!(f, "{}", chksum),
        }
    }
}

#[allow(dead_code)]
pub struct GcsLocation<'a> {
    bucket: &'a str,
    prefix: &'a str,
}

#[allow(dead_code)]
pub struct S3Location<'a> {
    pub bucket: &'a str,
    pub region: &'a str,
    pub host: &'a str,
    pub prefix: &'a str,
}

pub struct FilesystemLocation<'a> {
    pub path: &'a Path,
}

pub struct BlobLocation<'a> {
    pub prefix: &'a str,
    pub container: &'a str,
}

pub enum CloudLocation<'a> {
    Gcs(GcsLocation<'a>),
    S3(S3Location<'a>),
    Fs(FilesystemLocation<'a>),
    Blob(BlobLocation<'a>),
}

pub type Storage = Arc<dyn Backend + Sync + Send>;

pub struct Ctx {
    pub client: HttpClient,
    pub backend: Storage,
    pub krates: Vec<Krate>,
    pub registries: Vec<Arc<Registry>>,
    pub root_dir: PathBuf,
}

impl Ctx {
    pub fn new(
        root_dir: Option<PathBuf>,
        backend: Storage,
        krates: Vec<Krate>,
        registries: Vec<Arc<Registry>>,
    ) -> Result<Self, Error> {
        Ok(Self {
            client: HttpClient::builder().build()?,
            backend,
            krates,
            registries,
            root_dir: root_dir.unwrap_or_else(|| PathBuf::from(".")),
        })
    }

    /// Create the registry and git directories as they are the root of multiple other ones
    pub fn prep_sync_dirs(&self) -> Result<(), Error> {
        std::fs::create_dir_all(self.root_dir.join("registry"))?;
        std::fs::create_dir_all(self.root_dir.join("git"))?;

        Ok(())
    }

    pub fn registry_sets(&self) -> Vec<mirror::RegistrySet> {
        self.registries
            .iter()
            .map(|registry| {
                // Gather the names of all of the crates sourced in the registry so we
                // can add .cache entries
                let krates = self
                    .krates
                    .iter()
                    .filter_map(|krate| {
                        if krate == registry.as_ref() {
                            Some(krate.name.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                mirror::RegistrySet {
                    registry: registry.clone(),
                    krates,
                }
            })
            .collect()
    }
}

impl fmt::Debug for Ctx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "krates: {}", self.krates.len())
    }
}

pub type Timestamp = time::OffsetDateTime;

pub trait Backend: fmt::Debug {
    fn fetch(&self, krate: &Krate) -> Result<bytes::Bytes, Error>;
    fn upload(&self, source: bytes::Bytes, krate: &Krate) -> Result<usize, Error>;
    fn list(&self) -> Result<Vec<String>, Error>;
    fn updated(&self, krate: &Krate) -> Result<Option<Timestamp>, Error>;
    fn set_prefix(&mut self, prefix: &str);
}
