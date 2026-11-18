use super::*;

pub const IO_WRITE_ABORT: MessageKind = MessageKind {
    domain: Domain::Io,
    kind: 6,
};

pub(super) struct ChunkedWrite {
    id: u64,
    key: String,
    data: Vec<u8>,
    offset: usize,
    chunk_bytes: usize,
}

impl ChunkedWrite {
    fn metadata(id: u64, key: &str, size: usize, offset: usize) -> String {
        json!({
            "namespace": "save", "key": key, "transaction_id": id.to_string(),
            "size": size.to_string(), "offset": offset.to_string(),
        })
        .to_string()
    }

    fn end(&self) -> usize {
        self.offset + self.chunk_bytes.min(self.data.len() - self.offset)
    }

    pub(super) fn message(&self) -> (MessageKind, String, Vec<u8>) {
        (
            IO_WRITE_ATOMIC,
            Self::metadata(self.id, &self.key, self.data.len(), self.offset),
            self.data[self.offset..self.end()].to_vec(),
        )
    }

    pub(super) fn decode(
        &self,
        status: &str,
        metadata: &Value,
        blob: &[u8],
    ) -> Result<Completion, ProtocolError> {
        let end = self.end();
        let final_chunk = end == self.data.len();
        if !blob.is_empty()
            || status != if final_chunk { "ok" } else { "staged" }
            || decimal(metadata, "transaction_id")? != self.id
            || decimal(metadata, "next_offset")? != end as u64
        {
            return Err(invalid_reply());
        }
        Ok(if final_chunk {
            Completion::Written
        } else {
            Completion::WriteStaged
        })
    }

    pub(super) fn advance(&mut self) {
        self.offset = self.end();
    }
}

pub(super) fn write_metadata(key: &str) -> String {
    json!({"namespace":"save","key":key}).to_string()
}

impl ScriptIoSession {
    pub(super) fn write_file_limit(&self) -> usize {
        if self.chunked_writes {
            self.max_file_bytes
        } else {
            self.max_file_bytes.min(self.limits().blob_bytes)
        }
    }

    pub(super) fn prepare_write(&mut self) -> Result<(), ProtocolError> {
        let Some(Operation::Write { key, data }) = &self.operation else {
            return Ok(());
        };
        let size = data.len();
        if size > self.write_file_limit() {
            return Err(ProtocolError::Limit);
        }
        if self
            .write_payload_budget(&write_metadata(key))
            .is_some_and(|budget| size <= budget)
        {
            return Ok(());
        }
        if !self.chunked_writes || size == 0 {
            return Err(ProtocolError::Limit);
        }
        let id = self
            .last_write_transaction
            .checked_add(1)
            .ok_or(ProtocolError::Limit)?;
        // Reserve for the widest offset so every following chunk can fit an empty queue.
        let chunk_bytes = self
            .write_payload_budget(&ChunkedWrite::metadata(id, key, size, size))
            .filter(|bytes| *bytes > 0)
            .ok_or(ProtocolError::Limit)?;
        let Some(Operation::Write { key, data }) = self.operation.take() else {
            unreachable!();
        };
        self.last_write_transaction = id;
        self.operation = Some(Operation::ChunkedWrite(ChunkedWrite {
            id,
            key,
            data,
            offset: 0,
            chunk_bytes,
        }));
        Ok(())
    }

    fn write_payload_budget(&self, metadata: &str) -> Option<usize> {
        let limits = self.limits();
        if metadata.len() > limits.metadata_bytes {
            return None;
        }
        limits
            .control
            .bytes
            .min(u32::MAX as usize)
            .checked_sub(HEADER_BYTES + 8 + metadata.len())
            .map(|remaining| remaining.min(limits.blob_bytes))
    }

    pub(super) fn pending_write_abort(&self) -> Result<Option<MessageBody>, ProtocolError> {
        let Some(Operation::ChunkedWrite(write)) = &self.operation else {
            return Ok(None);
        };
        if write.offset == 0 && self.identity.is_none() {
            return Ok(None);
        }
        MessageBody::new(
            json!({"transaction_id":write.id.to_string()}).to_string(),
            vec![],
        )
        .map(Some)
    }
}
