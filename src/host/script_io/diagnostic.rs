use super::*;
use std::sync::{Arc, Mutex};

const SCRIPT_PRINT: u32 = 2;
const MAX_LINE_BYTES: usize = 4096;
const PRINT_RECORD_OVERHEAD: usize = HEADER_BYTES + 8 + "{}".len();

// Lua runs inside a step. Buffer its output until the step releases the VM.
#[derive(Default)]
pub(super) struct ScriptPrint(Arc<Mutex<Pending>>);

#[derive(Default)]
struct Pending {
    lines: Vec<Vec<u8>>,
    bytes: usize,
    dropped: u64,
}

impl ScriptPrint {
    pub(super) fn install(
        &self,
        interpreter: &Interpreter,
        limits: TransportLimits,
    ) -> Result<(), ScriptIoError> {
        let pending = Arc::clone(&self.0);
        let budget = limits.diagnostics;
        let max_bytes = MAX_LINE_BYTES
            .min(limits.blob_bytes)
            .min(budget.bytes - PRINT_RECORD_OVERHEAD);
        interpreter.set_print_handler(max_bytes, move |line| {
            let mut pending = pending.lock().unwrap();
            if let Some(line) = line {
                let bytes = PRINT_RECORD_OVERHEAD + line.len();
                if pending.lines.len() < budget.records && bytes <= budget.bytes - pending.bytes {
                    pending.bytes += bytes;
                    pending.lines.push(line);
                    return;
                }
            }
            pending.dropped = pending.dropped.saturating_add(1);
        })?;
        Ok(())
    }

    pub(super) fn flush(&self, transport: &mut SessionTransport) {
        let pending = std::mem::take(&mut *self.0.lock().unwrap());
        transport.count_dropped_diagnostics(pending.dropped);
        for line in pending.lines {
            if MessageBody::new("{}".into(), line)
                .and_then(|body| transport.diagnostic(SCRIPT_PRINT, body))
                .is_err()
            {
                transport.count_dropped_diagnostics(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_overflow_does_not_block_lua_and_reload_discards_pending_output() {
        let mut limits = TransportLimits::default();
        limits.diagnostics = QueueBudget {
            records: 1,
            bytes: PRINT_RECORD_OVERHEAD + 8,
        };
        let mut session =
            ScriptIoSession::new(1, InterpreterConfig::default(), limits, 1024).unwrap();
        session
            .open_chunk(
                b"print('first'); print('second'); print('too long for this budget')",
                "logs",
            )
            .unwrap();
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Completed);
        assert_eq!(session.state().diagnostics.records, 1);
        assert_eq!(session.state().dropped_diagnostics, 2);

        session.open_chunk(b"print('next')", "reload").unwrap();
        assert_eq!(session.step().unwrap(), ScriptIoProgress::Completed);
        let mut output = [0; 256];
        let poll = session.poll_events(&mut output);
        assert_eq!(poll.record_count, 2);
        assert!(output[..poll.written_bytes].ends_with(b"{}next"));
        assert_eq!(session.state().dropped_diagnostics, 2);
    }
}
