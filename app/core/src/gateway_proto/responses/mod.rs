//! 在既有 Chat Completions 接口和 Responses 上游之间转换。

mod output;
mod request;
mod stream;

pub use output::to_chat_completion;
pub use request::from_chat_request;
pub use stream::ResponsesChatStream;

#[cfg(test)]
mod tests;
