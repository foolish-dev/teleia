package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/foolish-dev/Teleia/go/internal/llm"
)

func Definitions() []llm.ToolDef {
	return []llm.ToolDef{
		{
			Type: "function",
			Function: llm.ToolDefFunction{
				Name:        "read",
				Description: "Read a file from disk. Returns the file contents as text.",
				Parameters: map[string]any{
					"type":       "object",
					"properties": map[string]any{"path": map[string]any{"type": "string"}},
					"required":   []string{"path"},
				},
			},
		},
		{
			Type: "function",
			Function: llm.ToolDefFunction{
				Name:        "write",
				Description: "Write contents to a file, creating or overwriting it.",
				Parameters: map[string]any{
					"type": "object",
					"properties": map[string]any{
						"path":    map[string]any{"type": "string"},
						"content": map[string]any{"type": "string"},
					},
					"required": []string{"path", "content"},
				},
			},
		},
		{
			Type: "function",
			Function: llm.ToolDefFunction{
				Name:        "edit",
				Description: "Replace a unique substring in a file. Fails if old_string is missing or non-unique.",
				Parameters: map[string]any{
					"type": "object",
					"properties": map[string]any{
						"path":       map[string]any{"type": "string"},
						"old_string": map[string]any{"type": "string"},
						"new_string": map[string]any{"type": "string"},
					},
					"required": []string{"path", "old_string", "new_string"},
				},
			},
		},
		{
			Type: "function",
			Function: llm.ToolDefFunction{
				Name:        "bash",
				Description: "Run a shell command and return its combined stdout/stderr. 30s timeout.",
				Parameters: map[string]any{
					"type":       "object",
					"properties": map[string]any{"command": map[string]any{"type": "string"}},
					"required":   []string{"command"},
				},
			},
		},
	}
}

func Dispatch(ctx context.Context, name, arguments string) (string, error) {
	switch name {
	case "read":
		var a struct {
			Path string `json:"path"`
		}
		if err := json.Unmarshal([]byte(arguments), &a); err != nil {
			return "", err
		}
		b, err := os.ReadFile(a.Path)
		return string(b), err
	case "write":
		var a struct {
			Path    string `json:"path"`
			Content string `json:"content"`
		}
		if err := json.Unmarshal([]byte(arguments), &a); err != nil {
			return "", err
		}
		if dir := filepath.Dir(a.Path); dir != "" && dir != "." {
			_ = os.MkdirAll(dir, 0o755)
		}
		if err := os.WriteFile(a.Path, []byte(a.Content), 0o644); err != nil {
			return "", err
		}
		return fmt.Sprintf("wrote %d bytes to %s", len(a.Content), a.Path), nil
	case "edit":
		var a struct {
			Path      string `json:"path"`
			OldString string `json:"old_string"`
			NewString string `json:"new_string"`
		}
		if err := json.Unmarshal([]byte(arguments), &a); err != nil {
			return "", err
		}
		raw, err := os.ReadFile(a.Path)
		if err != nil {
			return "", err
		}
		text := string(raw)
		n := strings.Count(text, a.OldString)
		if n == 0 {
			return "", fmt.Errorf("old_string not found in %s", a.Path)
		}
		if n > 1 {
			return "", fmt.Errorf("old_string matches %d times in %s; needs to be unique", n, a.Path)
		}
		updated := strings.Replace(text, a.OldString, a.NewString, 1)
		if err := os.WriteFile(a.Path, []byte(updated), 0o644); err != nil {
			return "", err
		}
		return "edited " + a.Path, nil
	case "bash":
		var a struct {
			Command string `json:"command"`
		}
		if err := json.Unmarshal([]byte(arguments), &a); err != nil {
			return "", err
		}
		bctx, cancel := context.WithTimeout(ctx, 30*time.Second)
		defer cancel()
		cmd := exec.CommandContext(bctx, "bash", "-lc", a.Command)
		out, err := cmd.CombinedOutput()
		text := string(out)
		if err != nil && bctx.Err() == context.DeadlineExceeded {
			return text + "\n[bash timed out after 30s]", nil
		}
		if cmd.ProcessState != nil && !cmd.ProcessState.Success() {
			text += fmt.Sprintf("\n[exit %d]", cmd.ProcessState.ExitCode())
		}
		return text, nil
	}
	return "", fmt.Errorf("unknown tool: %s", name)
}
