package agent

import (
	"context"
	"fmt"

	"github.com/foolish-dev/Teleia/go/internal/llm"
	"github.com/foolish-dev/Teleia/go/internal/store"
	"github.com/foolish-dev/Teleia/go/internal/tools"
)

const systemPrompt = "You are Teleia, a terse coding assistant running in a terminal. " +
	"Use the provided tools (read, write, edit, bash) to do real work. " +
	"Default to brief replies. When you finish a turn, stop — do not narrate."

const maxToolHops = 16

type TurnEvent interface{ isTurnEvent() }

type AssistantStart struct{}

func (AssistantStart) isTurnEvent() {}

type AssistantDelta struct{ Text string }

func (AssistantDelta) isTurnEvent() {}

type AssistantEnd struct{}

func (AssistantEnd) isTurnEvent() {}

type ToolStart struct{ Name, Arguments string }

func (ToolStart) isTurnEvent() {}

type ToolEnd struct{ Name, Output string }

func (ToolEnd) isTurnEvent() {}

type TurnEnd struct{}

func (TurnEnd) isTurnEvent() {}

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

func (a *Agent) Reset() error {
	id, err := a.store.CreateSession(a.llm.Model)
	if err != nil {
		return err
	}
	a.sessionID = id
	a.messages = nil
	a.seq = 0
	return a.push(llm.Message{Role: "system", Content: systemPrompt})
}

func (a *Agent) SaveAlias(name string) error {
	return a.store.SaveAlias(name, a.sessionID)
}

func (a *Agent) LoadAlias(name string) (string, error) {
	id, err := a.store.ResolveAlias(name)
	if err != nil {
		return "", err
	}
	msgs, err := a.store.Load(id)
	if err != nil {
		return "", err
	}
	a.sessionID = id
	a.messages = msgs
	a.seq = len(msgs)
	return id, nil
}

func (a *Agent) push(m llm.Message) error {
	if err := a.store.Append(a.sessionID, a.seq, m); err != nil {
		return err
	}
	a.seq++
	a.messages = append(a.messages, m)
	return nil
}

// Turn runs one turn and pushes events to the returned channel. Closed when done.
// Errors are surfaced as AssistantDelta entries with an "error:" prefix.
func (a *Agent) Turn(ctx context.Context, userInput string) (<-chan TurnEvent, error) {
	if err := a.push(llm.Message{Role: "user", Content: userInput}); err != nil {
		return nil, err
	}
	out := make(chan TurnEvent, 64)
	go func() {
		defer close(out)
		for hop := 0; hop < maxToolHops; hop++ {
			out <- AssistantStart{}
			var contentBuf string
			var toolCalls []llm.ToolCall

			stream, err := a.llm.Stream(ctx, a.messages, a.tools)
			if err != nil {
				out <- AssistantDelta{Text: fmt.Sprintf("error: %v", err)}
				out <- TurnEnd{}
				return
			}
			for ev := range stream {
				switch v := ev.(type) {
				case *llm.ContentDelta:
					contentBuf += v.Text
					out <- AssistantDelta{Text: v.Text}
				case *llm.StreamDone:
					toolCalls = v.ToolCalls
				}
			}
			out <- AssistantEnd{}

			assistant := llm.Message{Role: "assistant", Content: contentBuf, ToolCalls: toolCalls}
			if err := a.push(assistant); err != nil {
				out <- AssistantDelta{Text: fmt.Sprintf("error: %v", err)}
				out <- TurnEnd{}
				return
			}

			if len(toolCalls) == 0 {
				out <- TurnEnd{}
				return
			}

			for _, call := range toolCalls {
				out <- ToolStart{Name: call.Function.Name, Arguments: call.Function.Arguments}
				output, terr := tools.Dispatch(ctx, call.Function.Name, call.Function.Arguments)
				if terr != nil {
					output = fmt.Sprintf("error: %v", terr)
				}
				out <- ToolEnd{Name: call.Function.Name, Output: output}
				if err := a.push(llm.Message{Role: "tool", ToolCallID: call.ID, Content: output}); err != nil {
					out <- AssistantDelta{Text: fmt.Sprintf("error: %v", err)}
					out <- TurnEnd{}
					return
				}
			}
		}
		out <- AssistantDelta{Text: fmt.Sprintf("[stopped: hit tool-hop limit of %d]", maxToolHops)}
		out <- TurnEnd{}
	}()
	return out, nil
}
