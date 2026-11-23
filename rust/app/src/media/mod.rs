pub mod audio;
pub mod embeddings;
pub mod images;
pub mod videos;
pub mod web;

pub use audio::{audio_options_handler, speech_handler, transcriptions_handler};
pub use embeddings::embeddings_handler;
pub use images::image_generations_handler;
pub use videos::video_generations_handler;
pub use web::{systemone_handler, web_fetch_handler, web_search_handler};

