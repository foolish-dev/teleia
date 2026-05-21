package llm

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"sort"
	"strings"
	"time"
)

const DefaultBaseURL = "http://127.0.0.1:11434/v1"

type Message struct {
	Role       string     `json:"role"`
	Content    string     `json:"content"`
	ToolCalls  []ToolCall `json:"tool_calls,omitempty"`
	ToolCallID string     `json:"tool_call_id,omitempty"`
}

type ToolCall struct {
	ID       string           `json:"id"`
	Type     string           `json:"type"`
	Function ToolCallFunction `json:"function"`
}

type ToolCallFunction struct {
	Name      string `json:"name"`
	Arguments string `json:"arguments"`
}

type ToolDef struct {
	Type     string          `json:"type"`
	Function ToolDefFunction `json:"function"`
}

type ToolDefFunction struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	Parameters  map[string]any `json:"parameters"`
}

type Client struct {
	BaseURL string
	Model   string
	HTTP    *http.Client
}

func New(baseURL, model string) *Client {
	return &Client{
		BaseURL: strings.TrimRight(baseURL, "/"),
		Model:   model,
		HTTP:    &http.Client{Timeout: 5 * time.Minute},
	}
}

// StreamEvent is one of *ContentDelta or *StreamDone, sent on the channel.
type StreamEvent interface{ isStreamEvent() }

type ContentDelta struct{ Text string }

func (*ContentDelta) isStreamEvent() {}

type StreamDone struct{ ToolCalls []ToolCall }

func (*StreamDone) isStreamEvent() {}

type chatRequest struct {
	Model    string    `json:"model"`
	Messages []Message `json:"messages"`
	Tools    []ToolDef `json:"tools,omitempty"`
	Stream   bool      `json:"stream"`
}

type streamChunk struct {
	Choices []struct {
		Delta struct {
			Content   string `json:"content"`
			ToolCalls []struct {
				Index    int    `json:"index"`
				ID       string `json:"id"`
				Type     string `json:"type"`
				Function struct {
					Name      string `json:"name"`
					Arguments string `json:"arguments"`
				} `json:"function"`
			} `json:"tool_calls"`
		} `json:"delta"`
	} `json:"choices"`
}

// Stream opens a chat completion in stream mode and pushes events on the returned channel.
// If an error occurs, it's returned by Stream() before any events. The channel is closed
// after StreamDone is sent.
func (c *Client) Stream(ctx context.Context, messages []Message, tools []ToolDef) (<-chan StreamEvent, error) {
	body, err := json.Marshal(chatRequest{Model: c.Model, Messages: messages, Tools: tools, Stream: true})
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.BaseURL+"/chat/completions", bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("content-type", "application/json")

	resp, err := c.HTTP.Do(req)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode/100 != 2 {
		raw, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		return nil, fmt.Errorf("ollama %d: %s", resp.StatusCode, string(raw))
	}

	out := make(chan StreamEvent, 16)
	go func() {
		defer resp.Body.Close()
		defer close(out)

		scanner := bufio.NewScanner(resp.Body)
		scanner.Buffer(make([]byte, 64*1024), 1024*1024)
		acc := map[int]*ToolCallFunction{}
		ids := map[int]string{}
		types := map[int]string{}

		for scanner.Scan() {
			line := scanner.Text()
			if !strings.HasPrefix(line, "data:") {
				continue
			}
			payload := strings.TrimSpace(strings.TrimPrefix(line, "data:"))
			if payload == "" || payload == "[DONE]" {
				continue
			}
			var chunk streamChunk
			if err := json.Unmarshal([]byte(payload), &chunk); err != nil {
				continue
			}
			for _, choice := range chunk.Choices {
				if t := choice.Delta.Content; t != "" {
					out <- &ContentDelta{Text: t}
				}
				for _, tcd := range choice.Delta.ToolCalls {
					slot, ok := acc[tcd.Index]
					if !ok {
						slot = &ToolCallFunction{}
						acc[tcd.Index] = slot
					}
					if tcd.ID != "" {
						ids[tcd.Index] = tcd.ID
					}
					if tcd.Type != "" {
						types[tcd.Index] = tcd.Type
					}
					if tcd.Function.Name != "" {
						slot.Name = tcd.Function.Name
					}
					if tcd.Function.Arguments != "" {
						slot.Arguments += tcd.Function.Arguments
					}
				}
			}
		}

		indexes := make([]int, 0, len(acc))
		for k := range acc {
			indexes = append(indexes, k)
		}
		sort.Ints(indexes)

		tcs := make([]ToolCall, 0, len(indexes))
		for _, idx := range indexes {
			kind := types[idx]
			if kind == "" {
				kind = "function"
			}
			tcs = append(tcs, ToolCall{
				ID:       ids[idx],
				Type:     kind,
				Function: *acc[idx],
			})
		}
		out <- &StreamDone{ToolCalls: tcs}
	}()

	return out, nil
}
