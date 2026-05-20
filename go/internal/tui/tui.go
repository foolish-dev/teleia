package tui

import (
	"context"
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"

	"github.com/foolish-dev/Teleia/go/internal/agent"
)

var (
	userStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("#7dcfff")).Bold(true)
	assistantStyle = lipgloss.NewStyle().Foreground(lipgloss.Color("#bb9af7")).Bold(true)
	toolStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("#e0af68"))
	dimStyle       = lipgloss.NewStyle().Foreground(lipgloss.Color("#565f89"))
	errorStyle     = lipgloss.NewStyle().Foreground(lipgloss.Color("#f7768e"))
	promptStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("#7dcfff"))
	borderStyle    = lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).Padding(0, 1)
)

type entry struct {
	kind        string // user, assistant, tool, error
	text        string
	toolName    string
	toolArgs    string
	toolOutput  string
}

type stepsMsg struct {
	steps []agent.Step
	err   error
}

type model struct {
	agent   *agent.Agent
	width   int
	height  int
	input   string
	history []entry
	status  string
	working bool
}

func Run(a *agent.Agent) error {
	m := model{
		agent:  a,
		status: fmt.Sprintf("session %s ready · enter to send · ctrl-c to quit", a.SessionID()[:12]),
	}
	_, err := tea.NewProgram(m, tea.WithAltScreen()).Run()
	return err
}

func (m model) Init() tea.Cmd { return nil }

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
	case tea.KeyMsg:
		if m.working {
			return m, nil
		}
		switch msg.Type {
		case tea.KeyCtrlC:
			return m, tea.Quit
		case tea.KeyEnter:
			if strings.TrimSpace(m.input) == "" {
				return m, nil
			}
			prompt := m.input
			m.input = ""
			m.history = append(m.history, entry{kind: "user", text: prompt})
			m.working = true
			m.status = "thinking…"
			return m, runTurn(m.agent, prompt)
		case tea.KeyBackspace:
			if len(m.input) > 0 {
				m.input = m.input[:len(m.input)-1]
			}
		case tea.KeySpace:
			m.input += " "
		case tea.KeyRunes:
			m.input += string(msg.Runes)
		}
	case stepsMsg:
		m.working = false
		if msg.err != nil {
			m.history = append(m.history, entry{kind: "error", text: msg.err.Error()})
			m.status = "error · ready"
		} else {
			for _, s := range msg.steps {
				switch v := s.(type) {
				case agent.AssistantStep:
					m.history = append(m.history, entry{kind: "assistant", text: v.Text})
				case agent.ToolStep:
					m.history = append(m.history, entry{
						kind: "tool", toolName: v.Name, toolArgs: v.Arguments, toolOutput: v.Output,
					})
				}
			}
			m.status = "ready"
		}
	}
	return m, nil
}

func (m model) View() string {
	if m.width == 0 {
		return ""
	}
	logH := m.height - 4
	if logH < 3 {
		logH = 3
	}

	var lines []string
	for _, e := range m.history {
		switch e.kind {
		case "user":
			lines = append(lines, userStyle.Render("you"))
			lines = append(lines, e.text, "")
		case "assistant":
			lines = append(lines, assistantStyle.Render("teleia"))
			lines = append(lines, e.text, "")
		case "tool":
			lines = append(lines, toolStyle.Render(fmt.Sprintf("⚙ %s(%s)", e.toolName, e.toolArgs)))
			for i, l := range strings.Split(e.toolOutput, "\n") {
				if i >= 20 {
					break
				}
				lines = append(lines, dimStyle.Render("  "+l))
			}
			lines = append(lines, "")
		case "error":
			lines = append(lines, errorStyle.Render("error: "+e.text), "")
		}
	}

	start := 0
	if len(lines) > logH {
		start = len(lines) - logH
	}
	log := strings.Join(lines[start:], "\n")
	logBox := borderStyle.Width(m.width - 2).Height(logH).Render(log)

	inputLine := promptStyle.Render("> ") + m.input
	inputBox := borderStyle.Width(m.width - 2).Render(inputLine)

	status := dimStyle.Render(m.status)
	return lipgloss.JoinVertical(lipgloss.Left, logBox, inputBox, status)
}

func runTurn(a *agent.Agent, prompt string) tea.Cmd {
	return func() tea.Msg {
		steps, err := a.Turn(context.Background(), prompt)
		return stepsMsg{steps: steps, err: err}
	}
}
