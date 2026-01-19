use anyhow::Result;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
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
}

impl Block {
    pub fn raw_content(&self) -> String {
        match &self.kind {
            BlockKind::Code(_) | BlockKind::Output => {
                let lines: Vec<&str> = self.content.trim().lines().collect();
                if lines.len() >= 2
                    && lines[0].starts_with("```")
                    && lines.last().unwrap().starts_with("```")
                {
                    lines[1..lines.len() - 1].join("\n")
                } else {
                    self.content.clone()
                }
            }
            BlockKind::Markdown => self.content.clone(),
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

impl Notebook {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            blocks: Vec::new(),
        }
    }

    pub fn load_from_file(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path).unwrap_or_default();
        let mut blocks = Vec::new();

        let parser = Parser::new(&content).into_offset_iter();
        let mut last_offset = 0;
        let mut in_code_block = false;
        let mut code_lang = String::new();

        // Simple approach: Iterate events.
        // Ideally we use offsets to preserve raw markdown.
        // But for now, let's just parse what we can identify as code blocks.
        // Actually, for "load and execute", we just need to identify code blocks.
        // For "save", we append.

        // Strategy:
        // We will process the file to identify blocks.
        // If we want to preserve exact text between code blocks, we should capture the text ranges.

        for (event, range) in parser {
            match event {
                Event::Start(Tag::CodeBlock(kind)) => {
                    // Capture text before this code block as Markdown block
                    if range.start > last_offset {
                        let text = &content[last_offset..range.start];
                        if !text.is_empty() {
                            blocks.push(Block {
                                content: text.to_string(),
                                kind: BlockKind::Markdown,
                            });
                        }
                    }

                    in_code_block = true;
                    code_lang = match kind {
                        CodeBlockKind::Fenced(lang) => lang.to_string(),
                        CodeBlockKind::Indented => "".to_string(),
                    };
                    last_offset = range.start; // Mark start of code block content? 
                    // Wait, range includes fences for Fenced blocks?
                    // pulldown-cmark range covers the whole event.
                }
                Event::End(TagEnd::CodeBlock) => {
                    if in_code_block {
                        // The range end is the end of the block.
                        // However, we want the *content* of the code block.
                        // Actually, taking the whole range is better for reproduction,
                        // but we need inner content for execution.

                        let full_block = &content[last_offset..range.end];
                        // We need to strip fences if we want just code.
                        // This is getting complicated to do perfectly round-trip.

                        // Alternate simple strategy:
                        // Just store the whole range as "Code Block" and parse inner content when executing.
                        let kind = if code_lang == "text" {
                            BlockKind::Output
                        } else {
                            BlockKind::Code(code_lang.clone())
                        };

                        blocks.push(Block {
                            content: full_block.to_string(),
                            kind,
                        });

                        last_offset = range.end;
                        in_code_block = false;
                    }
                }
                _ => {}
            }
        }

        // Remaining text
        if last_offset < content.len() {
            let text = &content[last_offset..];
            blocks.push(Block {
                content: text.to_string(),
                kind: BlockKind::Markdown,
            });
        }

        Ok(Self {
            path: path.to_path_buf(),
            blocks,
        })
    }

    // Simple append for recording
    pub fn append_code(&mut self, code: &str) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        let block = format!("\n```bash\n{}\n```\n", code.trim());
        file.write_all(block.as_bytes())?;

        self.blocks.push(Block {
            content: block,
            kind: BlockKind::Code("bash".to_string()),
        });
        Ok(())
    }

    pub fn append_output(&mut self, output: &str) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        // Use a generic code block or specific generic one for output
        let block = format!("\n```text\n{}\n```\n", output.trim_end());
        file.write_all(block.as_bytes())?;

        self.blocks.push(Block {
            content: block,
            kind: BlockKind::Output,
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
        let notebook = if path.exists() {
            Notebook::load_from_file(&path)?
        } else {
            Notebook::new(path)
        };

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
