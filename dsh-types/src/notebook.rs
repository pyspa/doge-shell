use anyhow::Result;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum BlockKind {
    Code(String), // Language
    Markdown,
    Output,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub content: String,
    pub kind: BlockKind,
    // Decoded by the Markdown parser, independently of the original source used for saving.
    body: Option<String>,
}

impl Block {
    pub fn raw_content(&self) -> String {
        self.body.as_ref().unwrap_or(&self.content).clone()
    }

    fn markdown(content: &str) -> Self {
        Self {
            content: content.to_string(),
            kind: BlockKind::Markdown,
            body: None,
        }
    }
}

#[derive(Debug)]
pub struct Notebook {
    pub path: PathBuf,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    Active,
    Paused,
}

#[derive(Debug)]
pub struct NotebookSession {
    pub notebook: Notebook,
    pub state: SessionState,
}

/// A fence longer than any backtick run in the body, so it cannot close early.
pub fn fence_for(body: &str) -> String {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in body.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

impl Notebook {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            blocks: Vec::new(),
        }
    }

    pub fn load_from_file(path: &Path) -> Result<Self> {
        Self::read(path, File::open(path)?)
    }

    fn read(path: &Path, mut file: File) -> Result<Self> {
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let mut notebook = Self::new(path.to_owned());
        let mut last_offset = 0;
        let mut quote_depth = 0;
        let mut code = None;

        for (event, range) in Parser::new(&content).into_offset_iter() {
            match event {
                // Runbook exports put output in blockquotes. A fence in quoted
                // output is part of the prose, even when it is labelled `sh`.
                Event::Start(Tag::BlockQuote(_)) => quote_depth += 1,
                Event::End(TagEnd::BlockQuote(_)) => quote_depth -= 1,
                Event::Start(Tag::CodeBlock(kind)) if quote_depth == 0 => {
                    if range.start > last_offset {
                        notebook
                            .blocks
                            .push(Block::markdown(&content[last_offset..range.start]));
                    }
                    let kind = match kind {
                        CodeBlockKind::Fenced(lang) if lang.as_ref() == "text" => BlockKind::Output,
                        CodeBlockKind::Fenced(lang) => BlockKind::Code(lang.to_string()),
                        CodeBlockKind::Indented => BlockKind::Code(String::new()),
                    };
                    code = Some((kind, String::new()));
                    last_offset = range.start;
                }
                Event::Text(text) => {
                    if let Some((_, body)) = &mut code {
                        body.push_str(&text);
                    }
                }
                Event::End(TagEnd::CodeBlock) => {
                    if let Some((kind, mut body)) = code.take() {
                        if body.ends_with('\n') {
                            body.pop();
                        }
                        notebook.blocks.push(Block {
                            content: content[last_offset..range.end].to_string(),
                            kind,
                            body: Some(body),
                        });
                        last_offset = range.end;
                    }
                }
                _ => {}
            }
        }
        if last_offset < content.len() {
            notebook
                .blocks
                .push(Block::markdown(&content[last_offset..]));
        }
        Ok(notebook)
    }

    pub fn append_code(&mut self, code: &str) -> Result<()> {
        self.append_block(code.trim(), "bash", BlockKind::Code("bash".to_owned()))
    }

    pub fn append_output(&mut self, output: &str) -> Result<()> {
        self.append_block(output.trim_end(), "text", BlockKind::Output)
    }

    fn append_block(&mut self, body: &str, language: &str, kind: BlockKind) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let fence = fence_for(body);
        let content = format!("\n{fence}{language}\n{body}\n{fence}\n");
        file.write_all(content.as_bytes())?;
        self.blocks.push(Block {
            content,
            kind,
            body: Some(body.to_owned()),
        });
        Ok(())
    }

    pub fn save_to_file(&self) -> Result<()> {
        let mut file = File::create(&self.path)?;
        for block in &self.blocks {
            // content already contains fences for code blocks and raw text for markdown
            write!(file, "{}", block.content)?;
        }
        Ok(())
    }
}

impl NotebookSession {
    pub fn new(path: PathBuf) -> Result<Self> {
        // Anchor recording to the original directory even when a command runs `cd`.
        let path = std::path::absolute(path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        anyhow::ensure!(
            file.metadata()?.is_file(),
            "expected a regular notebook file"
        );
        file.rewind()?;
        let notebook = Notebook::read(&path, file)?;

        Ok(Self {
            notebook,
            state: SessionState::Active,
        })
    }

    pub fn pause(&mut self) {
        self.state = SessionState::Paused;
    }

    pub fn resume(&mut self) {
        self.state = SessionState::Active;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::NamedTempFile;

    #[test]
    fn test_notebook_save_load() -> Result<()> {
        let temp_file = NamedTempFile::new()?;
        let path = temp_file.path().to_path_buf();

        // 1. Create new notebook (session)
        let mut session = NotebookSession::new(path.clone())?;

        // 2. Append code (Active Mode)
        session.notebook.append_code("echo hello")?;
        session.notebook.append_output("hello")?;

        // 3. Verify file content matches
        let content = fs::read_to_string(&path)?;
        assert!(content.contains("```bash\necho hello\n```"));
        assert!(content.contains("```text\nhello\n```"));

        // 4. Load from file (Execution Mode check)
        let loaded = Notebook::load_from_file(&path)?;

        // Filter for meaningful blocks
        let meaningful_blocks: Vec<&Block> = loaded
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::Code(_) | BlockKind::Output))
            .collect();

        assert_eq!(meaningful_blocks.len(), 2);
        match &meaningful_blocks[0].kind {
            BlockKind::Code(lang) => assert_eq!(lang, "bash"),
            _ => panic!("Expected Code block"),
        }
        match &meaningful_blocks[1].kind {
            BlockKind::Output => {}
            _ => panic!("Expected Output block"),
        }

        // 5. Save loop (Idempotency)
        loaded.save_to_file()?;
        let content_2 = fs::read_to_string(&path)?;
        assert_eq!(content, content_2);

        Ok(())
    }

    #[test]
    fn recorded_code_and_output_keep_embedded_fences_in_their_blocks() -> Result<()> {
        let file = NamedTempFile::new()?;
        let code = "cat <<'EOF'\n```\nEOF";
        let output = "````\n```sh\nquoted output\n```";
        let mut session = NotebookSession::new(file.path().into())?;
        session.notebook.append_code(code)?;
        session.notebook.append_output(output)?;

        let loaded = Notebook::load_from_file(file.path())?;
        let blocks: Vec<_> = loaded
            .blocks
            .iter()
            .filter(|block| block.kind != BlockKind::Markdown)
            .map(|block| (block.kind.clone(), block.raw_content()))
            .collect();
        assert_eq!(
            blocks,
            vec![
                (BlockKind::Code("bash".into()), code.into()),
                (BlockKind::Output, output.into()),
            ]
        );
        Ok(())
    }

    #[test]
    fn test_load_complex_markdown() -> Result<()> {
        let temp_file = NamedTempFile::new()?;
        let path = temp_file.path().to_path_buf();

        let initial_content = r#"
# Header
Some text.

```rust
fn main() {}
```

More text.
"#;
        fs::write(&path, initial_content)?;

        let notebook = Notebook::load_from_file(&path)?;
        assert_eq!(notebook.blocks.len(), 3); // MD, Code, MD

        if let BlockKind::Markdown = &notebook.blocks[0].kind {
            assert!(notebook.blocks[0].content.contains("# Header"));
        } else {
            panic!("Block 0 should be Markdown");
        }

        if let BlockKind::Code(lang) = &notebook.blocks[1].kind {
            assert_eq!(lang, "rust");
            assert!(notebook.blocks[1].content.contains("fn main"));
        } else {
            panic!("Block 1 should be Code");
        }

        if let BlockKind::Markdown = &notebook.blocks[2].kind {
            assert!(notebook.blocks[2].content.contains("More text"));
        } else {
            panic!("Block 2 should be Markdown");
        }

        Ok(())
    }

    #[test]
    fn test_session_control() -> Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut session = NotebookSession::new(temp_file.path().into())?;

        assert_eq!(session.state, SessionState::Active);
        session.pause();
        assert_eq!(session.state, SessionState::Paused);
        session.resume();
        assert_eq!(session.state, SessionState::Active);

        Ok(())
    }
}
