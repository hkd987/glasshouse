//! First-party action providers. Every first-party app ships with full
//! action coverage (NV-3); these are the first two.

mod files;
mod mail;

pub use files::{FilesProvider, MAX_READ_BYTES};
pub use mail::MailProvider;

use std::path::Path;

use glass_platform::xattr;

use crate::TaskContext;

/// Tags a file the agent changed with "by [agent] for [user]" (AU-10).
/// Best effort: a filesystem without extended attributes still gets the
/// change, and the audit log still attributes it.
pub(crate) fn attribute(ctx: &TaskContext, path: &Path) {
    if let Err(e) = xattr::write_attribution(path, &ctx.principal.attribution()) {
        tracing::debug!(path = %path.display(), error = %e, "could not record attribution");
    }
}
