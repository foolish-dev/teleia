use anyhow::Result;
use async_stream::try_stream;
use futures_util::{pin_mut, Stream, StreamExt};
use teleia_llm::{ChatEvent, LlmClient, Message, ToolDef};
use teleia_store::Store;

const SYSTEM_PROMPT: &str = "You are Teleia, a terse coding assistant running in a terminal. \
Use the provided tools (read, write, edit, bash) to do real work. \
Default to brief replies. When you finish a turn, stop — do not narrate.";

pub const MAX_TOOL_HOPS: usize = 16;

/// Events emitted by `turn()`. The TUI consumes these to render incrementally.
#[derive(Debug, Clone)]
pub enum TurnEvent {
    AssistantStart,
    AssistantDelta(String),
    AssistantEnd,
    ToolStart { name: String, arguments: String },
    ToolEnd { name: String, output: String },
    TurnEnd,
}

pub struct Agent {
    llm: LlmClient,
    tools: Vec<ToolDef>,
    store: Store,
    session_id: String,
    messages: Vec<Message>,
    seq: usize,
}

impl Agent {
    pub fn new(llm: LlmClient, store: Store) -> Result<Self> {
        let session_id = store.create_session(llm.model())?;
        let mut agent = Self {
            llm,
            tools: teleia_tools::definitions(),
            store,
            session_id,
            messages: Vec::new(),
            seq: 0,
        };
        agent.push(Message::System {
            content: SYSTEM_PROMPT.to_string(),
        })?;
        Ok(agent)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn reset(&mut self) -> Result<()> {
        let session_id = self.store.create_session(self.llm.model())?;
        self.session_id = session_id;
        self.messages.clear();
        self.seq = 0;
        self.push(Message::System {
            content: SYSTEM_PROMPT.to_string(),
        })?;
        Ok(())
    }

    pub fn load_alias(&mut self, name: &str) -> Result<String> {
        let session_id = self.store.resolve_alias(name)?;
        let messages = self.store.load(&session_id)?;
        self.session_id = session_id.clone();
        self.messages = messages;
        self.seq = self.messages.len();
        Ok(session_id)
    }

    pub fn save_alias(&self, name: &str) -> Result<()> {
        self.store.save_alias(name, &self.session_id)
    }

    pub fn list_aliases(&self) -> Result<Vec<(String, String, i64)>> {
        self.store.list_aliases()
    }

    pub fn delete_alias(&self, name: &str) -> Result<()> {
        self.store.delete_alias(name)
    }

    pub fn model(&self) -> &str {
        self.llm.model()
    }

    pub fn set_model(&mut self, model: String) {
        self.llm.set_model(model);
    }

    pub fn turn<'a>(
        &'a mut self,
        user_input: String,
    ) -> impl Stream<Item = Result<TurnEvent>> + 'a {
        try_stream! {
            self.push(Message::User { content: user_input })?;

            for _ in 0..MAX_TOOL_HOPS {
                yield TurnEvent::AssistantStart;
                let mut content_buf = String::new();
                let mut tool_calls = Vec::new();

                {
                    let stream = self.llm.stream(&self.messages, Some(&self.tools));
                    pin_mut!(stream);
                    while let Some(event) = stream.next().await {
                        match event? {
                            ChatEvent::ContentDelta(text) => {
                                content_buf.push_str(&text);
                                yield TurnEvent::AssistantDelta(text);
                            }
                            ChatEvent::Done { tool_calls: tcs } => {
                                tool_calls = tcs;
                            }
                        }
                    }
                }

                yield TurnEvent::AssistantEnd;

                let assistant_msg = Message::Assistant {
                    content: (!content_buf.is_empty()).then_some(content_buf.clone()),
                    tool_calls: tool_calls.clone(),
                };
                self.push(assistant_msg)?;

                if tool_calls.is_empty() {
                    yield TurnEvent::TurnEnd;
                    return;
                }

                for call in tool_calls {
                    yield TurnEvent::ToolStart {
                        name: call.function.name.clone(),
                        arguments: call.function.arguments.clone(),
                    };
                    let output = match teleia_tools::dispatch(
                        &call.function.name,
                        &call.function.arguments,
                    )
                    .await
                    {
                        Ok(o) => o,
                        Err(e) => format!("error: {e}"),
                    };
                    yield TurnEvent::ToolEnd {
                        name: call.function.name.clone(),
                        output: output.clone(),
                    };
                    self.push(Message::Tool {
                        tool_call_id: call.id.clone(),
                        content: output,
                    })?;
                }
            }

            yield TurnEvent::AssistantStart;
            yield TurnEvent::AssistantDelta(format!(
                "[stopped: hit tool-hop limit of {MAX_TOOL_HOPS}]"
            ));
            yield TurnEvent::AssistantEnd;
            yield TurnEvent::TurnEnd;
        }
    }

    fn push(&mut self, message: Message) -> Result<()> {
        self.store.append(&self.session_id, self.seq, &message)?;
        self.seq += 1;
        self.messages.push(message);
        Ok(())
    }
}
