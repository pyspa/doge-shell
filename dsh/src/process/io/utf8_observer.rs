/// UTF-8 projection for read chunks. Only an incomplete character survives a
/// call; capture and display continue to consume the original bytes.
#[derive(Default)]
pub(super) struct Utf8ObserverDecoder {
    pending: [u8; 3],
    pending_len: usize,
}

impl Utf8ObserverDecoder {
    pub(super) fn push(&mut self, chunk: &[u8]) -> String {
        let mut bytes = Vec::with_capacity(self.pending_len + chunk.len());
        bytes.extend_from_slice(&self.pending[..self.pending_len]);
        bytes.extend_from_slice(chunk);
        self.pending_len = 0;
        let mut remaining = bytes.as_slice();
        let mut text = String::new();
        loop {
            match std::str::from_utf8(remaining) {
                Ok(valid) => {
                    text.push_str(valid);
                    break;
                }
                Err(error) => {
                    let (valid, tail) = remaining.split_at(error.valid_up_to());
                    text.push_str(std::str::from_utf8(valid).expect("validated UTF-8 prefix"));
                    match error.error_len() {
                        Some(len) => {
                            text.push('\u{fffd}');
                            remaining = &tail[len..];
                        }
                        None => {
                            // An incomplete UTF-8 scalar is at most three bytes.
                            self.pending[..tail.len()].copy_from_slice(tail);
                            self.pending_len = tail.len();
                            break;
                        }
                    }
                }
            }
        }
        text
    }

    pub(super) fn finish(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.pending[..self.pending_len]).into_owned();
        self.pending_len = 0;
        text
    }
}
