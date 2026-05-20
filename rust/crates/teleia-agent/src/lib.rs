use anyhow::Result;
use teleia_llm::{LlmClient, Message, ToolDef};
use teleia_store::Store;

const SYSTEM_PROMPT: &str = "You are Teleia, a terse coding assistant running in a terminal. \
Use the provided tools (read, write, edit, bash) to do real work. \
Default to brief replies. When you finish a turn, stop — do not narrate.";

const MAX_TOOL_HOPS: usize = 16;

pub struct Agent {
    llm: LlmClient,
    tools: Vec<ToolDef>,
    store: Store,
    session_id: String,
    messages: Vec<Message>,
    seq: usize,
}

pub enum Step {
    Assistant(String),
    Tool {
        name: String,
        input: String,
        output: String,
    },
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
        let system = Message::System {
            content: SYSTEM_PROMPT.to_string(),
        };
        agent.push(system)?;
        Ok(agent)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub async fn turn(&mut self, user_input: String) -> Result<Vec<Step>> {
        self.push(Message::User {
            content: user_input,
        })?;
        let mut steps = Vec::new();

        for _ in 0..MAX_TOOL_HOPS {
            let reply = self.llm.chat(&self.messages, Some(&self.tools)).await?;
            self.push(reply.clone())?;

            let Message::Assistant {
                content,
                tool_calls,
            } = reply
            else {
                continue;
            };

            if let Some(text) = content.as_ref() {
                if !text.is_empty() {
                    steps.push(Step::Assistant(text.clone()));
                }
            }

            if tool_calls.is_empty() {
                return Ok(steps);
            }

            for call in tool_calls {
                let output =
                    match teleia_tools::dispatch(&call.function.name, &call.function.arguments)
                        .await
                    {
                        Ok(o) => o,
                        Err(e) => format!("error: {e}"),
                    };
                steps.push(Step::Tool {
                    name: call.function.name.clone(),
                    input: call.function.arguments.clone(),
                    output: output.clone(),
                });
                self.push(Message::Tool {
                    tool_call_id: call.id.clone(),
                    content: output,
                })?;
            }
        }

        steps.push(Step::Assistant(format!(
            "[stopped: hit tool-hop limit of {MAX_TOOL_HOPS}]"
        )));
        Ok(steps)
    }

    fn push(&mut self, message: Message) -> Result<()> {
        self.store.append(&self.session_id, self.seq, &message)?;
        self.seq += 1;
        self.messages.push(message);
        Ok(())
    }
}
