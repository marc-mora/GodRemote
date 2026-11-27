//! Utilidades compartidas por las pruebas de integración.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use godremote::ssh::{PromptRequest, Prompter, SecretAnswer};

/// Respuestas automáticas a las preguntas que hace una conexión.
pub struct Answers {
    pub accept_host: bool,
    pub secrets: Mutex<Vec<String>>,
    pub host_prompts: AtomicUsize,
    pub secret_prompts: AtomicUsize,
    /// Textos de las preguntas recibidas.
    pub messages: Mutex<Vec<String>>,
}

impl Answers {
    pub fn hosts(&self) -> usize {
        self.host_prompts.load(Ordering::SeqCst)
    }

    pub fn secrets_asked(&self) -> usize {
        self.secret_prompts.load(Ordering::SeqCst)
    }
}

pub fn answers(accept_host: bool, secrets: &[&str]) -> Arc<Answers> {
    Arc::new(Answers {
        accept_host,
        secrets: Mutex::new(secrets.iter().map(|s| s.to_string()).collect()),
        host_prompts: AtomicUsize::new(0),
        secret_prompts: AtomicUsize::new(0),
        messages: Mutex::new(vec![]),
    })
}

/// Crea un `Prompter` que contesta solo, como lo haría el usuario.
pub fn prompter(answers: Arc<Answers>) -> Prompter {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(req) = rx.recv() {
            match req {
                PromptRequest::HostKey {
                    reply, fingerprint, ..
                } => {
                    assert!(fingerprint.starts_with("SHA256:"));
                    answers.host_prompts.fetch_add(1, Ordering::SeqCst);
                    let _ = reply.send(answers.accept_host);
                }
                PromptRequest::Secret { reply, message, .. } => {
                    answers.secret_prompts.fetch_add(1, Ordering::SeqCst);
                    answers.messages.lock().unwrap().push(message);
                    let mut s = answers.secrets.lock().unwrap();
                    let answer = if s.is_empty() {
                        None
                    } else {
                        Some(SecretAnswer {
                            value: s.remove(0),
                            save: true,
                        })
                    };
                    let _ = reply.send(answer);
                }
            }
        }
    });
    Prompter::new(tx, Arc::new(|| {}))
}
