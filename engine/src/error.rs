use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::c_char;

#[derive(Debug)]
pub enum EngineError {
    Io(String),
    Parse { pos: usize, message: String },
    Empty,
}

impl EngineError {
    pub fn parse(pos: usize, message: impl Into<String>) -> Self {
        EngineError::Parse { pos, message: message.into() }
    }

    pub fn message(&self) -> String {
        match self {
            EngineError::Io(m) => format!("I/O error: {}", m),
            EngineError::Parse { pos, message } => {
                format!("Parse error at byte {}: {}", pos, message)
            }
            EngineError::Empty => "Empty document".to_string(),
        }
    }
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

pub fn set_last_error(err: &EngineError) {
    let msg = CString::new(err.message())
        .unwrap_or_else(|_| CString::new("error").expect("ASCII contains no NUL"));
    LAST_ERROR.with(|cell| *cell.borrow_mut() = Some(msg));
}

pub fn last_error_ptr() -> *const c_char {
    LAST_ERROR.with(|cell| {
        let borrow = cell.borrow();
        match borrow.as_ref() {
            Some(s) => s.as_ptr(),
            None => std::ptr::null(),
        }
    })
}
