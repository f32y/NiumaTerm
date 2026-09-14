use std::fs;
use std::path::Path;

use crate::team::content::AttachmentReference;
use crate::team::storage::{StorageError, digest};

pub(crate) fn read_attachment(
    directory: &Path,
    reference: &AttachmentReference,
) -> Result<Vec<u8>, StorageError> {
    let bytes = fs::read(directory.join("attachments").join(reference.id.to_string()))?;

    if bytes.len() as u64 != reference.bytes || digest(&bytes) != reference.digest {
        return Err(StorageError::Invalid("attachment contents changed"));
    }

    Ok(bytes)
}
