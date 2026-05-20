package main

import (
	"flag"
	"fmt"
	"os"

	"github.com/foolish-dev/Teleia/go/internal/agent"
	"github.com/foolish-dev/Teleia/go/internal/llm"
	"github.com/foolish-dev/Teleia/go/internal/store"
	"github.com/foolish-dev/Teleia/go/internal/tui"
)

func main() {
	model := flag.String("model", "hf.co/FoolDev/Thanatos-27B:Q4_K_M", "Ollama model name")
	baseURL := flag.String("base-url", llm.DefaultBaseURL, "OpenAI-compatible base URL")
	flag.Parse()

	st, err := store.Open()
	if err != nil {
		fmt.Fprintln(os.Stderr, "store:", err)
		os.Exit(1)
	}
	defer st.Close()

	a, err := agent.New(llm.New(*baseURL, *model), st)
	if err != nil {
		fmt.Fprintln(os.Stderr, "agent:", err)
		os.Exit(1)
	}
	if err := tui.Run(a); err != nil {
		fmt.Fprintln(os.Stderr, "tui:", err)
		os.Exit(1)
	}
}
