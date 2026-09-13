use std::fs;

use crate::team::content::AttachmentReference;
#[cfg(test)]
use crate::team::identity::AttachmentId;
#[cfg(test)]
use crate::team::storage::atomic_write;
use crate::team::storage::records::digest;
use crate::team::storage::{RoomStore, StorageError};

impl RoomStore {
    #[cfg(test)]
    pub(crate) fn save_attachment(
        &self,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<AttachmentReference, StorageError> {
        if self.failed {
            return Err(StorageError::ReopenRequired);
        }

        if bytes.is_empty() {
            return Err(StorageError::Invalid("empty attachment"));
        }

        let directory = self.directory.join("attachments");

        fs::create_dir_all(&directory)?;

        let reference = AttachmentReference {
            id: AttachmentId::new(),
            media_type: media_type.into(),
            bytes: bytes.len() as u64,
            digest: digest(bytes),
        };

        atomic_write(&directory.join(reference.id.to_string()), bytes)?;

        Ok(reference)
    }

    pub fn read_attachment(
        &self,
        reference: &AttachmentReference,
    ) -> Result<Vec<u8>, StorageError> {
        let bytes = fs::read(
            self.directory
                .join("attachments")
                .join(reference.id.to_string()),
        )?;

        if bytes.len() as u64 != reference.bytes || digest(&bytes) != reference.digest {
            return Err(StorageError::Invalid("attachment contents changed"));
        }

        Ok(bytes)
    }
}
