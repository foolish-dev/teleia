// Tool definitions + dispatch via the shared rust binary `teleia-tools-bin`.
package tools

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strings"

	"github.com/foolish-dev/Teleia/go/internal/llm"
)

const binary = "teleia-tools-bin"

func Definitions() []llm.ToolDef {
	out, err := exec.Command(binary, "defs").Output()
	if err != nil {
		panic(fmt.Sprintf("%s defs: %v", binary, err))
	}
	var defs []llm.ToolDef
	if err := json.Unmarshal(out, &defs); err != nil {
		panic(fmt.Sprintf("parse %s defs: %v", binary, err))
	}
	return defs
}

// Dispatch runs a tool via teleia-tools-bin. The binary encodes tool failures
// in stdout (always exits 0), so wrappers don't need to distinguish dispatch
// errors from non-zero bash exit codes — those are already in the output.
// A non-zero exit here means the binary itself failed (e.g., not on PATH).
func Dispatch(ctx context.Context, name, arguments string) (string, error) {
	cmd := exec.CommandContext(ctx, binary, "run", name)
	cmd.Stdin = strings.NewReader(arguments)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		return fmt.Sprintf("error: %s", strings.TrimSpace(stderr.String())), nil
	}
	return stdout.String(), nil
}
