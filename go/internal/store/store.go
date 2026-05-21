package store

import (
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/foolish-dev/Teleia/go/internal/llm"
	_ "modernc.org/sqlite"
)

type Store struct {
	db *sql.DB
}

func Open() (*Store, error) {
	path, err := dataPath()
	if err != nil {
		return nil, err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	const schema = `
		CREATE TABLE IF NOT EXISTS sessions (
			id TEXT PRIMARY KEY,
			model TEXT NOT NULL,
			created_at INTEGER NOT NULL
		);
		CREATE TABLE IF NOT EXISTS messages (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			session_id TEXT NOT NULL,
			seq INTEGER NOT NULL,
			payload TEXT NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
		CREATE TABLE IF NOT EXISTS aliases (
			name TEXT PRIMARY KEY,
			session_id TEXT NOT NULL,
			created_at INTEGER NOT NULL
		);
	`
	if _, err := db.Exec(schema); err != nil {
		return nil, err
	}
	return &Store{db: db}, nil
}

func (s *Store) Close() error { return s.db.Close() }

func (s *Store) CreateSession(model string) (string, error) {
	id := newID()
	_, err := s.db.Exec(
		"INSERT INTO sessions (id, model, created_at) VALUES (?, ?, ?)",
		id, model, time.Now().Unix(),
	)
	return id, err
}

func (s *Store) Append(sessionID string, seq int, m llm.Message) error {
	payload, err := json.Marshal(m)
	if err != nil {
		return err
	}
	_, err = s.db.Exec(
		"INSERT INTO messages (session_id, seq, payload) VALUES (?, ?, ?)",
		sessionID, seq, string(payload),
	)
	return err
}

func (s *Store) Load(sessionID string) ([]llm.Message, error) {
	rows, err := s.db.Query(
		"SELECT payload FROM messages WHERE session_id = ? ORDER BY seq ASC",
		sessionID,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []llm.Message
	for rows.Next() {
		var payload string
		if err := rows.Scan(&payload); err != nil {
			return nil, err
		}
		var m llm.Message
		if err := json.Unmarshal([]byte(payload), &m); err != nil {
			return nil, err
		}
		out = append(out, m)
	}
	return out, rows.Err()
}

func (s *Store) SaveAlias(name, sessionID string) error {
	_, err := s.db.Exec(
		"INSERT OR REPLACE INTO aliases (name, session_id, created_at) VALUES (?, ?, ?)",
		name, sessionID, time.Now().Unix(),
	)
	return err
}

func (s *Store) ResolveAlias(name string) (string, error) {
	var id string
	err := s.db.QueryRow("SELECT session_id FROM aliases WHERE name = ?", name).Scan(&id)
	if err == sql.ErrNoRows {
		return "", fmt.Errorf("no session saved as '%s'", name)
	}
	return id, err
}

func dataPath() (string, error) {
	if x := os.Getenv("XDG_DATA_HOME"); x != "" {
		return filepath.Join(x, "teleia", "teleia.sqlite"), nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".local", "share", "teleia", "teleia.sqlite"), nil
}

func newID() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		return fmt.Sprintf("%d", time.Now().UnixNano())
	}
	return hex.EncodeToString(b[:])
}
