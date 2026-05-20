package agent

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/foolish-dev/Teleia/go/internal/llm"
	"github.com/foolish-dev/Teleia/go/internal/store"
	"github.com/foolish-dev/Teleia/go/internal/tools"
)

const systemPrompt = "You are Teleia, a terse coding assistant running in a terminal. " +
	"Use the provided tools (read, write, edit, bash) to do real work. " +
	"Default to brief replies. When you finish a turn, stop — do not narrate."

const maxToolHops = 16

type Step interface{ isStep() }

type AssistantStep struct{ Text string }

func (AssistantStep) isStep() {}

type ToolStep struct{ Name, Arguments, Output string }

func (ToolStep) isStep() {}

type Agent struct {
	llm       *llm.Client
	tools     []llm.ToolDef
	store     *store.Store
	sessionID string
	messages  []llm.Message
	seq       int
}

func New(client *llm.Client, st *store.Store) (*Agent, error) {
	id, err := st.CreateSession(client.Model)
	if err != nil {
		return nil, err
	}
	a := &Agent{llm: client, tools: tools.Definitions(), store: st, sessionID: id}
	if err := a.push(llm.Message{Role: "system", Content: systemPrompt}); err != nil {
		return nil, err
	}
	return a, nil
}

func (a *Agent) SessionID() string { return a.sessionID }

func (a *Agent) Turn(ctx context.Context, userInput string) ([]Step, error) {
	if err := a.push(llm.Message{Role: "user", Content: userInput}); err != nil {
		return nil, err
	}
	var steps []Step

	for hop := 0; hop < maxToolHops; hop++ {
		reply, err := a.llm.Chat(ctx, a.messages, a.tools)
		if err != nil {
			return steps, err
		}
		if err := a.push(reply); err != nil {
			return steps, err
		}

		if reply.Content != "" {
			steps = append(steps, AssistantStep{Text: reply.Content})
		}

		if len(reply.ToolCalls) == 0 {
			return steps, nil
		}

		for _, call := range reply.ToolCalls {
			out, err := tools.Dispatch(ctx, call.Function.Name, call.Function.Arguments)
			if err != nil {
				out = fmt.Sprintf("error: %v", err)
			}
			steps = append(steps, ToolStep{Name: call.Function.Name, Arguments: call.Function.Arguments, Output: out})
			if err := a.push(llm.Message{Role: "tool", ToolCallID: call.ID, Content: out}); err != nil {
				return steps, err
			}
		}
	}
	steps = append(steps, AssistantStep{Text: fmt.Sprintf("[stopped: hit tool-hop limit of %d]", maxToolHops)})
	return steps, nil
}

func (a *Agent) push(m llm.Message) error {
	payload, err := json.Marshal(m)
	if err != nil {
		return err
	}
	if err := a.store.Append(a.sessionID, a.seq, m, string(payload)); err != nil {
		return err
	}
	a.seq++
	a.messages = append(a.messages, m)
	return nil
}
