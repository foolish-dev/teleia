package tui

import (
	"context"
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"

	"github.com/foolish-dev/Teleia/go/internal/agent"
)

const hints = "enter send · ↑↓ scroll · /help cmds · ctrl-c quit"

var (
	userStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("#7dcfff")).Bold(true)
	assistantStyle = lipgloss.NewStyle().Foreground(lipgloss.Color("#bb9af7")).Bold(true)
	toolStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("#e0af68"))
	dimStyle       = lipgloss.NewStyle().Foreground(lipgloss.Color("#565f89"))
	errorStyle     = lipgloss.NewStyle().Foreground(lipgloss.Color("#f7768e"))
	infoStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("#7aa2f7"))
	promptStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("#7dcfff"))
	borderStyle    = lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).Padding(0, 1)
)

type entry struct {
	kind     string // user, assistant, tool, error, info
	text     string
	complete bool
	tool     toolDetails
}

type toolDetails struct {
	name      string
	arguments string
	output    string
}

type eventMsg struct{ ev agent.TurnEvent }

type errMsg struct{ err error }

type model struct {
	agent   *agent.Agent
	width   int
	height  int
	input   string
	history []entry
	status  string
	working bool
	scroll  int
	stream  <-chan agent.TurnEvent
}

func Run(a *agent.Agent) error {
	m := model{
		agent:  a,
		status: fmt.Sprintf("session %s · ready", shortID(a.SessionID())),
	}
	_, err := tea.NewProgram(m, tea.WithAltScreen()).Run()
	return err
}

func shortID(s string) string {
	if len(s) <= 12 {
		return s
	}
	return s[:12]
}

func (m model) Init() tea.Cmd { return nil }

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height

	case eventMsg:
		m.applyEvent(msg.ev)
		if _, isEnd := msg.ev.(agent.TurnEnd); isEnd {
			m.working = false
			m.stream = nil
			m.status = fmt.Sprintf("session %s · ready", shortID(m.agent.SessionID()))
			return m, nil
		}
		return m, recvEvent(m.stream)

	case errMsg:
		m.history = append(m.history, entry{kind: "error", text: msg.err.Error()})
		m.working = false
		m.stream = nil
		m.status = "error · ready"
		return m, nil

	case tea.KeyMsg:
		if msg.Type == tea.KeyCtrlC {
			return m, tea.Quit
		}
		if m.working {
			return m, nil
		}
		switch msg.Type {
		case tea.KeyUp:
			m.scroll++
		case tea.KeyDown:
			if m.scroll > 0 {
				m.scroll--
			}
		case tea.KeyPgUp:
			m.scroll += 5
		case tea.KeyPgDown:
			if m.scroll > 5 {
				m.scroll -= 5
			} else {
				m.scroll = 0
			}
		case tea.KeyEnter:
			raw := strings.TrimSpace(m.input)
			m.input = ""
			if raw == "" {
				return m, nil
			}
			if strings.HasPrefix(raw, "/") {
				m.handleSlash(raw[1:])
				return m, nil
			}
			m.history = append(m.history, entry{kind: "user", text: raw})
			m.scroll = 0
			m.working = true
			m.status = "thinking…"
			stream, err := m.agent.Turn(context.Background(), raw)
			if err != nil {
				return m, func() tea.Msg { return errMsg{err: err} }
			}
			m.stream = stream
			return m, recvEvent(stream)
		case tea.KeyBackspace:
			if len(m.input) > 0 {
				m.input = m.input[:len(m.input)-1]
			}
		case tea.KeySpace:
			m.input += " "
		case tea.KeyRunes:
			m.input += string(msg.Runes)
		}
	}
	return m, nil
}

func recvEvent(stream <-chan agent.TurnEvent) tea.Cmd {
	return func() tea.Msg {
		ev, ok := <-stream
		if !ok {
			return eventMsg{ev: agent.TurnEnd{}}
		}
		return eventMsg{ev: ev}
	}
}

func (m *model) applyEvent(ev agent.TurnEvent) {
	switch v := ev.(type) {
	case agent.AssistantStart:
		m.history = append(m.history, entry{kind: "assistant", text: "", complete: false})
	case agent.AssistantDelta:
		if n := len(m.history); n > 0 && m.history[n-1].kind == "assistant" && !m.history[n-1].complete {
			m.history[n-1].text += v.Text
		} else {
			m.history = append(m.history, entry{kind: "assistant", text: v.Text})
		}
		m.scroll = 0
	case agent.AssistantEnd:
		if n := len(m.history); n > 0 && m.history[n-1].kind == "assistant" {
			m.history[n-1].complete = true
			if m.history[n-1].text == "" {
				m.history = m.history[:n-1]
			}
		}
	case agent.ToolStart:
		m.history = append(m.history, entry{
			kind: "tool", tool: toolDetails{name: v.Name, arguments: v.Arguments},
		})
		m.scroll = 0
	case agent.ToolEnd:
		if n := len(m.history); n > 0 && m.history[n-1].kind == "tool" {
			m.history[n-1].tool.output = v.Output
			m.history[n-1].complete = true
			m.scroll = 0
		}
	}
}

func (m *model) handleSlash(cmd string) {
	name, _, _ := strings.Cut(cmd, " ")
	name = strings.TrimSpace(name)
	arg := strings.TrimSpace(strings.TrimPrefix(cmd, name))

	push := func(kind, text string) {
		m.history = append(m.history, entry{kind: kind, text: text})
		m.scroll = 0
	}

	switch name {
	case "reset":
		if err := m.agent.Reset(); err != nil {
			push("error", "reset: "+err.Error())
			return
		}
		m.history = nil
		push("info", "started new session "+shortID(m.agent.SessionID()))
	case "save":
		if arg == "" {
			push("error", "usage: /save NAME")
			return
		}
		if err := m.agent.SaveAlias(arg); err != nil {
			push("error", "save: "+err.Error())
			return
		}
		push("info", "saved current session as '"+arg+"'")
	case "load":
		if arg == "" {
			push("error", "usage: /load NAME")
			return
		}
		id, err := m.agent.LoadAlias(arg)
		if err != nil {
			push("error", "load: "+err.Error())
			return
		}
		m.history = nil
		push("info", "loaded '"+arg+"' → session "+shortID(id))
	case "help", "?":
		push("info", "commands: /reset · /save NAME · /load NAME · /help")
	default:
		push("error", "unknown command: /"+name)
	}
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
		lines = append(lines, renderEntry(e)...)
	}

	maxOffset := len(lines) - logH
	if maxOffset < 0 {
		maxOffset = 0
	}
	start := maxOffset - m.scroll
	if start < 0 {
		start = 0
	}
	end := start + logH
	if end > len(lines) {
		end = len(lines)
	}
	log := strings.Join(lines[start:end], "\n")
	logBox := borderStyle.Width(m.width - 2).Height(logH).Render(log)

	inputLine := promptStyle.Render("> ") + m.input
	inputBox := borderStyle.Width(m.width - 2).Render(inputLine)

	status := dimStyle.Render(m.status + "   " + hints)
	return lipgloss.JoinVertical(lipgloss.Left, logBox, inputBox, status)
}

func renderEntry(e entry) []string {
	var out []string
	switch e.kind {
	case "user":
		out = append(out, userStyle.Render("you"))
		out = append(out, e.text, "")
	case "assistant":
		header := "teleia"
		if !e.complete {
			header = "teleia ▌"
		}
		out = append(out, assistantStyle.Render(header))
		out = append(out, e.text, "")
	case "tool":
		marker := "⚙"
		if !e.complete {
			marker = "⚙ …"
		}
		out = append(out, toolStyle.Render(fmt.Sprintf("%s %s(%s)", marker, e.tool.name, e.tool.arguments)))
		count := 0
		for _, l := range strings.Split(e.tool.output, "\n") {
			if count >= 20 {
				break
			}
			out = append(out, dimStyle.Render("  "+l))
			count++
		}
		out = append(out, "")
	case "error":
		out = append(out, errorStyle.Render("error: "+e.text), "")
	case "info":
		out = append(out, infoStyle.Render("· "+e.text), "")
	}
	return out
}
