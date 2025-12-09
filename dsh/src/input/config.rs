use crossterm::style::Color;

#[derive(Debug, Clone)]
pub struct InputConfig {
    pub fg_color: Color,                 // Normal input text (white)
    pub command_exists_color: Color,     // Command that exists (blue)
    pub command_not_exists_color: Color, // Command that doesn't exist (red)
    pub argument_color: Color,           // Arguments (cyan)
    pub variable_color: Color,           // Variables (yellow)
    pub single_quote_color: Color,       // Single quoted strings (green)
    pub double_quote_color: Color,       // Double quoted strings (Green with bold?)
    pub redirect_color: Color,           // Redirect operators (magenta)
    pub operator_color: Color,           // Logical/sequential operators
    pub pipe_color: Color,               // Pipe symbol
    pub background_color: Color,         // Background operator
    pub proc_subst_color: Color,         // Process substitution markers
    pub error_color: Color,              // Parse errors (red intense)
    pub completion_color: Color,         // Completion candidates (dark grey)
    pub ghost_color: Color,              // Inline suggestion text (dim gray)
    pub valid_path_color: Color,         // Valid path (magenta)
}

impl Default for InputConfig {
    fn default() -> InputConfig {
        InputConfig {
            fg_color: Color::White,
            command_exists_color: Color::Blue,
            command_not_exists_color: Color::Red,
            argument_color: Color::Cyan,
            variable_color: Color::Yellow,
            single_quote_color: Color::DarkGreen,
            double_quote_color: Color::Green,
            redirect_color: Color::Magenta,
            operator_color: Color::DarkYellow,
            pipe_color: Color::DarkCyan,
            background_color: Color::DarkMagenta,
            proc_subst_color: Color::DarkBlue,
            error_color: Color::Red,
            completion_color: Color::DarkGrey,
            ghost_color: Color::DarkGrey,
            valid_path_color: Color::Magenta,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ColorType {
    CommandExists,
    CommandNotExists,
    Argument,
    Variable,
    SingleQuote,
    DoubleQuote,
    Redirect,
    Operator,
    Pipe,
    Background,
    ProcSubst,
    Error,
    ValidPath,
}
