/// Multi-scenario integration tests for file read caching (Optimization 4).
///
/// Validates that the file read caching:
/// 1. Correctly identifies and deduplicates redundant reads within a single request
/// 2. Does NOT incorrectly replace content when only one copy exists (safety)
/// 3. Handles file modifications (cache invalidation) correctly
/// 4. Works across diverse real-world usage patterns
/// 5. Actually reduces token counts significantly
///
/// Each scenario simulates a realistic LLM agent conversation pattern.

use merlint::models::api::*;
use merlint::proxy::transformer::RequestTransformer;

// ── Helpers ─────────────────────────────────────────────────────────

fn tool(name: &str) -> Tool {
    Tool {
        tool_type: Some("function".into()),
        function: Some(FunctionDef {
            name: name.into(),
            description: Some(format!("{} tool", name)),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                }
            })),
        }),
        extra: serde_json::Map::new(),
    }
}

fn sys(content: &str) -> Message {
    Message {
        role: "system".into(),
        content: Some(MessageContent::Text(content.into())),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

fn user(content: &str) -> Message {
    Message {
        role: "user".into(),
        content: Some(MessageContent::Text(content.into())),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

fn asst(content: &str) -> Message {
    Message {
        role: "assistant".into(),
        content: Some(MessageContent::Text(content.into())),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

fn asst_read(call_id: &str, file_path: &str) -> Message {
    Message {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: Some(call_id.into()),
            call_type: Some("function".into()),
            function: Some(FunctionCall {
                name: "ReadFile".into(),
                arguments: format!(r#"{{"filePath":"{}"}}"#, file_path),
            }),
        }]),
        tool_call_id: None,
        name: None,
    }
}

fn asst_read_with_name(call_id: &str, file_path: &str, tool_name: &str) -> Message {
    Message {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: Some(call_id.into()),
            call_type: Some("function".into()),
            function: Some(FunctionCall {
                name: tool_name.into(),
                arguments: format!(r#"{{"file_path":"{}"}}"#, file_path),
            }),
        }]),
        tool_call_id: None,
        name: None,
    }
}

fn tool_result(call_id: &str, content: &str) -> Message {
    Message {
        role: "tool".into(),
        content: Some(MessageContent::Text(content.into())),
        tool_calls: None,
        tool_call_id: Some(call_id.into()),
        name: None,
    }
}

fn asst_write(call_id: &str, file_path: &str, content: &str) -> Message {
    Message {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: Some(call_id.into()),
            call_type: Some("function".into()),
            function: Some(FunctionCall {
                name: "WriteFile".into(),
                arguments: format!(
                    r#"{{"filePath":"{}","content":"{}"}}"#,
                    file_path,
                    content.replace('"', "\\\"").replace('\n', "\\n")
                ),
            }),
        }]),
        tool_call_id: None,
        name: None,
    }
}

fn req(tools: Vec<Tool>, messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: Some("gpt-4".into()),
        messages,
        tools,
        extra: serde_json::Map::new(),
    }
}

fn count_chars(request: &ChatRequest) -> usize {
    request.messages.iter().map(|m| {
        m.content.as_ref().map(|c| c.as_text().len()).unwrap_or(0)
    }).sum()
}

// ── Realistic file contents ─────────────────────────────────────────

const SERVER_PY: &str = r#"from flask import Flask, request, jsonify
import sqlite3
import os
import hashlib
import secrets
import re
from werkzeug.utils import secure_filename

app = Flask(__name__)
DB_PATH = "app.db"
UPLOAD_DIR = "/uploads"
ALLOWED_EXTENSIONS = {'.txt', '.pdf', '.png', '.jpg', '.jpeg', '.gif', '.csv', '.json'}

sessions = {}

def get_db():
    conn = sqlite3.connect(DB_PATH)
    conn.row_factory = sqlite3.Row
    return conn

def hash_password(password):
    salt = secrets.token_hex(16)
    pwdhash = hashlib.sha256((password + salt).encode()).hexdigest()
    return f"{salt}${pwdhash}"

def verify_password(stored_password, provided_password):
    try:
        salt, hash_value = stored_password.split('$')
        pwdhash = hashlib.sha256((provided_password + salt).encode()).hexdigest()
        return pwdhash == hash_value
    except ValueError:
        return False

@app.route('/api/users', methods=['GET'])
def list_users():
    db = get_db()
    cursor = db.execute("SELECT id, username FROM users")
    users = [dict(row) for row in cursor.fetchall()]
    db.close()
    return jsonify(users)

@app.route('/api/users', methods=['POST'])
def create_user():
    data = request.json
    if not data or 'username' not in data or 'password' not in data:
        return jsonify({"error": "username and password required"}), 400
    username = data['username']
    password = data['password']
    if not re.match(r'^[a-zA-Z0-9_]{3,50}$', username):
        return jsonify({"error": "invalid username format"}), 400
    if len(password) < 6:
        return jsonify({"error": "password must be at least 6 characters"}), 400
    hashed_password = hash_password(password)
    db = get_db()
    try:
        db.execute("INSERT INTO users (username, password) VALUES (?, ?)", (username, hashed_password))
        db.commit()
        return jsonify({"status": "created"}), 201
    except sqlite3.IntegrityError:
        db.close()
        return jsonify({"error": "username already exists"}), 409

@app.route('/api/search', methods=['GET'])
def search():
    query = request.args.get('q', '')
    sanitized_query = re.sub(r'[^\w\s\-_.]', '', query)
    db = get_db()
    search_pattern = f"%{sanitized_query}%"
    results = db.execute("SELECT * FROM products WHERE name LIKE ?", (search_pattern,)).fetchall()
    db.close()
    return jsonify([dict(row) for row in results])

@app.route('/api/upload', methods=['POST'])
def upload_file():
    if 'file' not in request.files:
        return jsonify({"error": "no file provided"}), 400
    f = request.files['file']
    if f.filename == '':
        return jsonify({"error": "empty filename"}), 400
    secure_name = secure_filename(f.filename)
    filepath = os.path.join(UPLOAD_DIR, secure_name)
    f.save(filepath)
    return jsonify({"status": "uploaded", "filename": secure_name})

if __name__ == '__main__':
    app.run(debug=True, host='0.0.0.0')
"#;

const AUDIT_TARGET_GO: &str = r#"package main

import (
    "database/sql"
    "fmt"
    "html/template"
    "io/ioutil"
    "log"
    "net/http"
    "os"
    "os/exec"
    "strings"
)

var db *sql.DB
var SECRET_KEY = "hardcoded-secret-key-12345"

func main() {
    http.HandleFunc("/", homeHandler)
    http.HandleFunc("/exec", execHandler)
    http.HandleFunc("/read", readHandler)
    http.HandleFunc("/template", templateHandler)
    http.HandleFunc("/redirect", redirectHandler)
    log.Fatal(http.ListenAndServe(":8080", nil))
}

func execHandler(w http.ResponseWriter, r *http.Request) {
    cmd := r.URL.Query().Get("cmd")
    output, err := exec.Command("sh", "-c", cmd).Output()
    if err != nil {
        http.Error(w, err.Error(), 500)
        return
    }
    w.Write(output)
}

func readHandler(w http.ResponseWriter, r *http.Request) {
    filename := r.URL.Query().Get("file")
    data, err := ioutil.ReadFile(filename)
    if err != nil {
        http.Error(w, "File not found", 404)
        return
    }
    w.Write(data)
}

func templateHandler(w http.ResponseWriter, r *http.Request) {
    name := r.URL.Query().Get("name")
    tmpl := fmt.Sprintf(`<html><body>Hello %s!</body></html>`, name)
    t, _ := template.New("page").Parse(tmpl)
    t.Execute(w, nil)
}

func redirectHandler(w http.ResponseWriter, r *http.Request) {
    target := r.URL.Query().Get("url")
    http.Redirect(w, r, target, http.StatusFound)
}

func homeHandler(w http.ResponseWriter, r *http.Request) {
    cookie, _ := r.Cookie("session")
    if cookie != nil {
        parts := strings.Split(cookie.Value, ":")
        if len(parts) == 2 {
            fmt.Fprintf(w, "Welcome back, %s", parts[0])
            return
        }
    }
    fmt.Fprintf(w, "Please login")
}

func connectDB() {
    var err error
    db, err = sql.Open("mysql", "root:password123@tcp(localhost:3306)/app")
    if err != nil {
        log.Fatal(err)
    }
}

func generateToken() string {
    return fmt.Sprintf("%d", os.Getpid())
}
"#;

const TRAIN_PY: &str = r#"import numpy as np
import json
import os
import pickle
import sys

class StandardScaler:
    def __init__(self):
        self.mean_ = None
        self.std_ = None

    def fit(self, X):
        self.mean_ = np.mean(X, axis=0)
        self.std_ = np.std(X, axis=0)
        self.std_[self.std_ == 0] = 1.0
        return self

    def transform(self, X):
        if self.mean_ is None or self.std_ is None:
            raise ValueError("Scaler must be fitted before transform")
        return (X - self.mean_) / self.std_

    def fit_transform(self, X):
        return self.fit(X).transform(X)

class SimpleClassifier:
    EPS = 1e-15

    def __init__(self, learning_rate=0.01, epochs=100):
        self.lr = learning_rate
        self.epochs = epochs
        self.weights = None
        self.bias = None
        self.history = []
        self.scaler = StandardScaler()

    def sigmoid(self, z):
        out = np.empty_like(z, dtype=float)
        pos_mask = z >= 0
        neg_mask = ~pos_mask
        out[pos_mask] = 1 / (1 + np.exp(-z[pos_mask]))
        exp_z = np.exp(z[neg_mask])
        out[neg_mask] = exp_z / (1 + exp_z)
        return out

    def fit(self, X, y):
        X = self.scaler.fit_transform(X)
        n_samples, n_features = X.shape
        self.weights = np.zeros(n_features)
        self.bias = 0
        for epoch in range(self.epochs):
            z = np.dot(X, self.weights) + self.bias
            predictions = self.sigmoid(z)
            dw = (1 / n_samples) * np.dot(X.T, (predictions - y))
            db = (1 / n_samples) * np.sum(predictions - y)
            self.weights -= self.lr * dw
            self.bias -= self.lr * db
            predictions_clipped = np.clip(predictions, self.EPS, 1 - self.EPS)
            loss = -np.mean(y * np.log(predictions_clipped) + (1 - y) * np.log(1 - predictions_clipped))
            accuracy = np.mean((predictions >= 0.5) == y)
            self.history.append({'epoch': epoch, 'loss': float(loss), 'accuracy': float(accuracy)})

    def predict(self, X):
        X = self.scaler.transform(X)
        z = np.dot(X, self.weights) + self.bias
        return (self.sigmoid(z) >= 0.5).astype(int)

    def predict_proba(self, X):
        X = self.scaler.transform(X)
        z = np.dot(X, self.weights) + self.bias
        return self.sigmoid(z)

def generate_dataset(n_samples=1000, n_features=10, seed=42):
    np.random.seed(seed)
    X = np.random.randn(n_samples, n_features)
    true_weights = np.random.randn(n_features)
    z = np.dot(X, true_weights) + np.random.randn(n_samples) * 0.5
    y = (z > 0).astype(float)
    return X, y

def evaluate_model(model, X_test, y_test):
    predictions = model.predict(X_test)
    probas = model.predict_proba(X_test)
    accuracy = np.mean(predictions == y_test)
    tp = np.sum((predictions == 1) & (y_test == 1))
    fp = np.sum((predictions == 1) & (y_test == 0))
    fn = np.sum((predictions == 0) & (y_test == 1))
    precision = tp / (tp + fp) if (tp + fp) > 0 else 0.0
    recall = tp / (tp + fn) if (tp + fn) > 0 else 0.0
    f1 = 2 * (precision * recall) / (precision + recall) if (precision + recall) > 0 else 0.0
    return {'accuracy': float(accuracy), 'precision': float(precision), 'recall': float(recall), 'f1': float(f1)}

def train_pipeline(config_path='config.json'):
    with open(config_path) as f:
        config = json.load(f)
    X, y = generate_dataset(n_samples=config['n_samples'], n_features=config['n_features'], seed=config.get('seed', 42))
    split = int(0.8 * len(X))
    X_train, X_test = X[:split], X[split:]
    y_train, y_test = y[:split], y[split:]
    model = SimpleClassifier(learning_rate=config.get('learning_rate', 0.01), epochs=config.get('epochs', 100))
    model.fit(X_train, y_train)
    metrics = evaluate_model(model, X_test, y_test)
    return model, metrics

if __name__ == '__main__':
    config_path = sys.argv[1] if len(sys.argv) > 1 else 'config.json'
    train_pipeline(config_path)
"#;

const ROUTES_TS: &str = r#"import express from 'express';
import jwt from 'jsonwebtoken';
import bcrypt from 'bcrypt';
import { authMiddleware, requireRole, requireOwnershipOrAdmin, validateRequest, validators, AuthenticatedRequest } from './middleware';

const router = express.Router();
const JWT_SECRET = process.env.JWT_SECRET || 'super-secret-key';
const TOKEN_EXPIRY = '24h';

interface User {
  id: number;
  email: string;
  password: string;
  role: 'admin' | 'user';
}

const users: User[] = [];
let nextId = 1;

function sanitizeUser(user: User) {
  const { password, ...safeUser } = user;
  return safeUser;
}

router.post('/register', validateRequest(validators.register), async (req: AuthenticatedRequest, res) => {
    const { email, password } = req.body;
    const existingUser = users.find(u => u.email === email);
    if (existingUser) return res.status(409).json({ error: 'Registration failed' });
    const hashedPassword = await bcrypt.hash(password, 10);
    const user: User = { id: nextId++, email, password: hashedPassword, role: 'user' };
    users.push(user);
    const token = jwt.sign({ id: user.id, role: user.role }, JWT_SECRET, { expiresIn: TOKEN_EXPIRY });
    res.status(201).json({ user: sanitizeUser(user), token });
});

router.post('/login', validateRequest(validators.login), async (req: AuthenticatedRequest, res) => {
    const { email, password } = req.body;
    const user = users.find(u => u.email === email);
    if (!user || !await bcrypt.compare(password, user.password)) return res.status(401).json({ error: 'Invalid credentials' });
    const token = jwt.sign({ id: user.id, role: user.role }, JWT_SECRET, { expiresIn: TOKEN_EXPIRY });
    res.json({ token });
});

router.get('/profile', authMiddleware, (req: AuthenticatedRequest, res) => {
    const user = users.find(u => u.id === req.userId);
    if (!user) return res.status(404).json({ error: 'User not found' });
    res.json(sanitizeUser(user));
});

router.get('/admin/users', authMiddleware, requireRole('admin'), (req: AuthenticatedRequest, res) => {
    res.json(users.map(sanitizeUser));
});

router.delete('/users/:id', authMiddleware, requireOwnershipOrAdmin, (req: AuthenticatedRequest, res) => {
    const userId = parseInt(req.params.id, 10);
    if (isNaN(userId)) return res.status(400).json({ error: 'Invalid user ID' });
    const idx = users.findIndex(u => u.id === userId);
    if (idx === -1) return res.status(404).json({ error: 'User not found' });
    if (req.userId === userId) return res.status(400).json({ error: 'Cannot delete your own account' });
    users.splice(idx, 1);
    res.json({ status: 'deleted' });
});

export default router;
"#;

const STUDENTS_CSV: &str = r#"student_id,name,class,math_midterm,math_final,english_midterm,english_final,science_midterm,science_final,attendance_rate,homework_completion,behavior_notes
S001,张明,三年级一班,85,88,78,82,80,85,0.95,0.92,学习态度认真，课堂参与积极
S002,李华,三年级一班,92,95,88,90,85,88,0.98,0.95,成绩优异，乐于助人
S003,王芳,三年级一班,75,72,70,68,72,70,0.85,0.80,学习较为被动，需要督促
S004,刘强,三年级一班,60,55,65,58,62,55,0.75,0.65,经常缺课，作业完成情况差
S005,陈静,三年级一班,88,92,90,93,87,90,0.96,0.94,全面发展，班级骨干
S006,赵伟,三年级一班,70,75,72,78,68,74,0.90,0.85,本学期有明显进步
S007,孙丽,三年级一班,82,78,85,80,83,79,0.88,0.82,成绩有所下滑，需要关注
S008,周杰,三年级一班,55,50,58,52,60,55,0.70,0.60,出勤率低，学习基础薄弱
S009,吴敏,三年级一班,90,88,92,89,88,86,0.94,0.90,成绩稳定，表现良好
S010,郑涛,三年级一班,68,70,65,68,70,72,0.92,0.88,学习踏实，稳步提升
"#;

const NGINX_CONF: &str = r#"upstream backend {
    server 127.0.0.1:8080;
    server 127.0.0.1:8081;
    keepalive 32;
}

server {
    listen 80;
    server_name example.com;
    return 301 https://$server_name$request_uri;
}

server {
    listen 443 ssl http2;
    server_name example.com;
    ssl_certificate /etc/ssl/certs/example.com.pem;
    ssl_certificate_key /etc/ssl/private/example.com.key;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers HIGH:!aNULL:!MD5;
    ssl_prefer_server_ciphers on;

    location / {
        proxy_pass http://backend;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_connect_timeout 5s;
        proxy_send_timeout 30s;
        proxy_read_timeout 30s;
    }

    location /static {
        root /var/www/html;
        expires 30d;
        add_header Cache-Control "public, no-transform";
    }

    location /api {
        proxy_pass http://backend;
        proxy_set_header Host $host;
        limit_req zone=api burst=20 nodelay;
    }
}
"#;

const DOCKERFILE: &str = r#"FROM node:18-alpine AS builder
WORKDIR /app
COPY package*.json ./
RUN npm ci --only=production
COPY . .
RUN npm run build

FROM node:18-alpine
RUN addgroup -g 1001 -S appgroup && adduser -S appuser -u 1001 -G appgroup
WORKDIR /app
COPY --from=builder /app/dist ./dist
COPY --from=builder /app/node_modules ./node_modules
COPY --from=builder /app/package.json ./
EXPOSE 3000
USER appuser
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://localhost:3000/health || exit 1
CMD ["node", "dist/index.js"]
"#;

const PRD_MD: &str = r#"# Product Requirements Document: Smart Task Manager

## 1. Overview
Smart Task Manager is an AI-powered task management application designed for small teams (5-20 people). The primary goal is to reduce the cognitive overhead of task prioritization and assignment.

## 2. Target Users
- Small startup teams
- Freelance project managers
- Remote teams needing async coordination

## 3. Core Features

### 3.1 Task Creation & Organization
- Users can create tasks with title, description, due date, and priority
- Tasks can be organized into projects and sub-projects
- Support for task dependencies (blocked-by relationships)
- Tags and custom fields for flexible categorization

### 3.2 AI-Powered Prioritization
- Automatic priority suggestions based on deadlines, dependencies, and team workload
- Smart notifications for tasks at risk of missing deadlines
- Weekly digest summarizing team progress and blockers

### 3.3 Team Collaboration
- Real-time commenting on tasks
- @mentions for team members
- Activity feed showing recent changes
- Role-based access control (Admin, Member, Viewer)

## 4. Technical Requirements
- Web application (React + Node.js)
- Mobile-responsive design
- REST API for integrations
- OAuth 2.0 authentication
- PostgreSQL database
- Redis for caching and real-time features

## 5. Success Metrics
- Task completion rate > 80%
- Average task creation time < 30 seconds
- User retention at 30 days > 60%
- NPS score > 40

## 6. Timeline
- Phase 1 (MVP): 8 weeks - Core task management
- Phase 2: 4 weeks - AI prioritization
- Phase 3: 4 weeks - Team collaboration features
"#;

// ── Scenario Tests ──────────────────────────────────────────────────

/// Scenario 1: Backend Developer - Bug fix workflow
/// Pattern: Read file → analyze → read same file again to implement fix
/// Expected: Second read of same file gets cached
#[test]
fn scenario_backend_dev_bug_fix() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("ReadFile"), tool("WriteFile"), tool("Bash"), tool("Grep")];

    let messages = vec![
        sys("You are a backend developer assistant."),
        user("Fix the SQL injection vulnerability in server.py"),
        // Agent reads the file to understand the code
        asst_read("c1", "/src/server.py"),
        tool_result("c1", SERVER_PY),
        asst("I found a potential SQL injection in the search endpoint. Let me read the file again to implement the fix precisely."),
        // Agent reads the SAME file again (common pattern when implementing a fix)
        asst_read("c2", "/src/server.py"),
        tool_result("c2", SERVER_PY),
        asst("Now I'll apply the fix."),
    ];

    let original_chars = count_chars(&req(tools.clone(), messages.clone()));
    let result = tx.transform(req(tools, messages));
    let optimized_chars = count_chars(&result.request);

    println!("[Scenario 1: Backend Dev Bug Fix]");
    println!("  Original chars:  {}", original_chars);
    println!("  Optimized chars: {}", optimized_chars);
    println!("  Chars saved:     {}", original_chars - optimized_chars);
    println!("  Est tokens saved: {}", result.estimated_tokens_saved);
    println!("  Cache hits:      {}", tx.file_cache_hits);

    assert_eq!(tx.file_cache_hits, 1, "Second read of server.py should be a cache hit");
    assert!(result.estimated_tokens_saved > 0, "Should save tokens");
    // The second SERVER_PY content (~2700 chars) should be replaced with ~120 char summary
    assert!(original_chars - optimized_chars > 2000, "Should save significant chars");
}

/// Scenario 2: Security Researcher - Multi-file audit
/// Pattern: Read multiple files, re-read some during report writing
/// Expected: Re-reads of unchanged files get cached
#[test]
fn scenario_security_audit() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("ReadFile"), tool("WriteFile"), tool("Bash")];

    let messages = vec![
        sys("You are a security researcher. Audit the code for vulnerabilities."),
        user("Audit audit_target.go for OWASP top 10 vulnerabilities"),
        // First read of Go file
        asst_read("c1", "/src/audit_target.go"),
        tool_result("c1", AUDIT_TARGET_GO),
        asst("I found several critical vulnerabilities. Let me also check the Python server for comparison."),
        // Read Python file
        asst_read("c2", "/src/server.py"),
        tool_result("c2", SERVER_PY),
        asst("Now let me re-examine the Go file to write the final report with exact line references."),
        // Re-read Go file (same content)
        asst_read("c3", "/src/audit_target.go"),
        tool_result("c3", AUDIT_TARGET_GO),
        asst("And confirm the Python server patterns..."),
        // Re-read Python file (same content)
        asst_read("c4", "/src/server.py"),
        tool_result("c4", SERVER_PY),
    ];

    let original_chars = count_chars(&req(tools.clone(), messages.clone()));
    let result = tx.transform(req(tools, messages));
    let optimized_chars = count_chars(&result.request);

    println!("\n[Scenario 2: Security Audit]");
    println!("  Original chars:  {}", original_chars);
    println!("  Optimized chars: {}", optimized_chars);
    println!("  Chars saved:     {}", original_chars - optimized_chars);
    println!("  Est tokens saved: {}", result.estimated_tokens_saved);
    println!("  Cache hits:      {}", tx.file_cache_hits);

    assert_eq!(tx.file_cache_hits, 2, "Both re-reads should be cache hits");
    // audit_target.go (~1700) + server.py (~2700) = ~4400 chars saved
    assert!(original_chars - optimized_chars > 3000, "Should save significant chars");
}

/// Scenario 3: ML Engineer - Read config, train, read config again
/// Pattern: Read config + code → run → read again to verify
/// Expected: Re-read of config gets cached; different files are independent
#[test]
fn scenario_ml_training() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("ReadFile"), tool("Bash"), tool("WriteFile")];

    let config_json = r#"{"n_samples": 1000, "n_features": 10, "learning_rate": 0.01, "epochs": 200, "seed": 42, "batch_size": 32, "validation_split": 0.2, "early_stopping": true, "patience": 10}"#;

    let messages = vec![
        sys("You are an ML engineer."),
        user("Review the training pipeline and config, then run training."),
        // Read train.py
        asst_read("c1", "/ml/train.py"),
        tool_result("c1", TRAIN_PY),
        // Read config
        asst_read("c2", "/ml/config.json"),
        tool_result("c2", config_json),
        asst("The config looks good. Let me re-read the training code to double-check the data loading."),
        // Re-read train.py (same content)
        asst_read("c3", "/ml/train.py"),
        tool_result("c3", TRAIN_PY),
        asst("Everything checks out. Now let me verify the config one more time."),
        // Re-read config (same content)
        asst_read("c4", "/ml/config.json"),
        tool_result("c4", config_json),
    ];

    let original_chars = count_chars(&req(tools.clone(), messages.clone()));
    let result = tx.transform(req(tools, messages));
    let optimized_chars = count_chars(&result.request);

    println!("\n[Scenario 3: ML Training Pipeline]");
    println!("  Original chars:  {}", original_chars);
    println!("  Optimized chars: {}", optimized_chars);
    println!("  Chars saved:     {}", original_chars - optimized_chars);
    println!("  Est tokens saved: {}", result.estimated_tokens_saved);
    println!("  Cache hits:      {}", tx.file_cache_hits);

    // config.json is <200 chars, so it won't be cached. But train.py will.
    assert!(tx.file_cache_hits >= 1, "Re-read of train.py should be cached");
    assert!(result.estimated_tokens_saved > 0);
}

/// Scenario 4: Fullstack Developer - Read API + Frontend, iterate on both
/// Pattern: Read multiple files in different languages, modify one, re-read both
/// Expected: Only the unmodified file gets cached; modified file keeps full content
#[test]
fn scenario_fullstack_dev() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("ReadFile"), tool("WriteFile"), tool("Bash")];

    let messages = vec![
        sys("You are a fullstack developer."),
        user("Add rate limiting to the API routes and update the frontend accordingly."),
        // Read routes
        asst_read("c1", "/api/routes.ts"),
        tool_result("c1", ROUTES_TS),
        // Read server.py too
        asst_read("c2", "/src/server.py"),
        tool_result("c2", SERVER_PY),
        // Write to routes (modify it)
        asst_write("c3", "/api/routes.ts", "// modified routes"),
        tool_result("c3", "File written successfully"),
        asst("I've updated routes.ts. Let me re-read both files to verify."),
        // Re-read routes (MODIFIED — content changed)
        asst_read("c4", "/api/routes.ts"),
        tool_result("c4", &format!("{}\n// Added rate limiting middleware", ROUTES_TS)),
        // Re-read server.py (UNCHANGED)
        asst_read("c5", "/src/server.py"),
        tool_result("c5", SERVER_PY),
    ];

    let original_chars = count_chars(&req(tools.clone(), messages.clone()));
    let result = tx.transform(req(tools, messages));
    let optimized_chars = count_chars(&result.request);

    println!("\n[Scenario 4: Fullstack Dev]");
    println!("  Original chars:  {}", original_chars);
    println!("  Optimized chars: {}", optimized_chars);
    println!("  Chars saved:     {}", original_chars - optimized_chars);
    println!("  Est tokens saved: {}", result.estimated_tokens_saved);
    println!("  Cache hits:      {}", tx.file_cache_hits);

    // server.py was read twice with same content => cache hit
    assert_eq!(tx.file_cache_hits, 1, "Only server.py should be cached (routes.ts changed)");

    // Verify modified routes.ts keeps its full content
    let routes_v2 = &result.request.messages.iter()
        .find(|m| m.tool_call_id.as_deref() == Some("c4"))
        .unwrap();
    let content = routes_v2.content.as_ref().unwrap().as_text();
    assert!(content.contains("rate limiting"), "Modified routes.ts should keep full content");
}

/// Scenario 5: DevOps Engineer - Read config files, verify, re-read
/// Pattern: Multiple config files, some re-read during deployment verification
#[test]
fn scenario_devops_deployment() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("ReadFile"), tool("Bash"), tool("WriteFile")];

    let messages = vec![
        sys("You are a DevOps engineer."),
        user("Review the deployment configuration and fix any issues."),
        // Read nginx config
        asst_read("c1", "/etc/nginx/nginx.conf"),
        tool_result("c1", NGINX_CONF),
        // Read Dockerfile
        asst_read("c2", "/app/Dockerfile"),
        tool_result("c2", DOCKERFILE),
        asst("I see issues with the nginx config. Let me also re-read it carefully for the SSL settings."),
        // Re-read nginx config (same)
        asst_read("c3", "/etc/nginx/nginx.conf"),
        tool_result("c3", NGINX_CONF),
        asst("Let me verify the Dockerfile once more for the health check."),
        // Re-read Dockerfile (same)
        asst_read("c4", "/app/Dockerfile"),
        tool_result("c4", DOCKERFILE),
    ];

    let original_chars = count_chars(&req(tools.clone(), messages.clone()));
    let result = tx.transform(req(tools, messages));
    let optimized_chars = count_chars(&result.request);

    println!("\n[Scenario 5: DevOps Deployment]");
    println!("  Original chars:  {}", original_chars);
    println!("  Optimized chars: {}", optimized_chars);
    println!("  Chars saved:     {}", original_chars - optimized_chars);
    println!("  Est tokens saved: {}", result.estimated_tokens_saved);
    println!("  Cache hits:      {}", tx.file_cache_hits);

    assert_eq!(tx.file_cache_hits, 2, "Both nginx.conf and Dockerfile re-reads should be cached");
}

/// Scenario 6: Teacher - Read student data, analyze, re-read for report
/// Pattern: Non-programmer reading CSV data multiple times
#[test]
fn scenario_teacher_analysis() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("ReadFile"), tool("WriteFile")];

    let messages = vec![
        sys("You are a helpful assistant for a teacher."),
        user("Analyze the student grades in students.csv and write a report."),
        asst_read("c1", "/data/students.csv"),
        tool_result("c1", STUDENTS_CSV),
        asst("I can see the student data. There are 10 students. Let me calculate averages. But first, let me re-read the data to make sure I have the exact numbers right."),
        asst_read("c2", "/data/students.csv"),
        tool_result("c2", STUDENTS_CSV),
        asst("Now let me re-read one more time to write the parent letters with exact grades."),
        asst_read("c3", "/data/students.csv"),
        tool_result("c3", STUDENTS_CSV),
    ];

    let original_chars = count_chars(&req(tools.clone(), messages.clone()));
    let result = tx.transform(req(tools, messages));
    let optimized_chars = count_chars(&result.request);

    println!("\n[Scenario 6: Teacher Analysis]");
    println!("  Original chars:  {}", original_chars);
    println!("  Optimized chars: {}", optimized_chars);
    println!("  Chars saved:     {}", original_chars - optimized_chars);
    println!("  Est tokens saved: {}", result.estimated_tokens_saved);
    println!("  Cache hits:      {}", tx.file_cache_hits);

    assert_eq!(tx.file_cache_hits, 2, "2nd and 3rd reads of students.csv should be cached");
}

/// Scenario 7: Product Manager - Read PRD, discuss, re-read for refinement
/// Pattern: Reading a long document, having conversation, then re-reading
#[test]
fn scenario_product_manager() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("ReadFile"), tool("WriteFile")];

    let messages = vec![
        sys("You are a product management assistant."),
        user("Review the PRD and suggest improvements."),
        asst_read("c1", "/docs/prd.md"),
        tool_result("c1", PRD_MD),
        asst("I've reviewed the PRD. Here are my suggestions: 1) Add more specific acceptance criteria 2) Include competitor analysis 3) Define the MVP scope more clearly."),
        user("Good points. Can you re-read the PRD and rewrite section 3 with your suggestions?"),
        asst_read("c2", "/docs/prd.md"),
        tool_result("c2", PRD_MD),
    ];

    let original_chars = count_chars(&req(tools.clone(), messages.clone()));
    let result = tx.transform(req(tools, messages));
    let optimized_chars = count_chars(&result.request);

    println!("\n[Scenario 7: Product Manager]");
    println!("  Original chars:  {}", original_chars);
    println!("  Optimized chars: {}", optimized_chars);
    println!("  Chars saved:     {}", original_chars - optimized_chars);
    println!("  Est tokens saved: {}", result.estimated_tokens_saved);
    println!("  Cache hits:      {}", tx.file_cache_hits);

    assert_eq!(tx.file_cache_hits, 1, "Re-read of PRD should be cached");
}

/// Scenario 8: Safety - Cross-request with truncated messages
/// Pattern: File read in request 1, then request 2 has truncated history
/// Expected: File should NOT be replaced (safety check)
#[test]
fn scenario_safety_truncated_history() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("ReadFile")];

    // Request 1: Normal read
    let req1 = req(tools.clone(), vec![
        sys("You are a coding assistant."),
        user("Read server.py"),
        asst_read("c1", "/src/server.py"),
        tool_result("c1", SERVER_PY),
    ]);
    tx.transform(req1);

    // Request 2: History was truncated by the framework
    // The old read is gone, only new read exists
    let req2 = req(tools.clone(), vec![
        sys("You are a coding assistant."),
        // Old messages truncated...
        user("Read server.py again to fix the bug"),
        asst_read("c2", "/src/server.py"),
        tool_result("c2", SERVER_PY),
    ]);

    let original_chars = count_chars(&req2);
    let result = tx.transform(req2);
    let optimized_chars = count_chars(&result.request);

    println!("\n[Scenario 8: Safety - Truncated History]");
    println!("  Original chars:  {}", original_chars);
    println!("  Optimized chars: {}", optimized_chars);
    println!("  Chars saved:     {}", original_chars - optimized_chars);
    println!("  Cache hits:      {}", tx.file_cache_hits);

    // Should NOT replace — only one copy in this request
    assert_eq!(tx.file_cache_hits, 0, "Should NOT cache when only one copy exists in request");
    assert_eq!(original_chars, optimized_chars, "Content should be preserved");
}

/// Scenario 9: Safety - File modified between reads
/// Pattern: Read → Write → Read (same path, different content)
/// Expected: Both reads keep full content (different hashes)
#[test]
fn scenario_safety_file_modified() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("ReadFile"), tool("WriteFile")];

    let modified_server = format!("{}\n# FIXED: Added input validation", SERVER_PY);

    let messages = vec![
        sys("You are a coding assistant."),
        user("Fix the bug in server.py"),
        asst_read("c1", "/src/server.py"),
        tool_result("c1", SERVER_PY),
        asst_write("c2", "/src/server.py", "fixed content"),
        tool_result("c2", "Written successfully"),
        asst("Let me verify the fix."),
        asst_read("c3", "/src/server.py"),
        tool_result("c3", &modified_server),
    ];

    let original_chars = count_chars(&req(tools.clone(), messages.clone()));
    let result = tx.transform(req(tools, messages));
    let optimized_chars = count_chars(&result.request);

    println!("\n[Scenario 9: Safety - File Modified Between Reads]");
    println!("  Original chars:  {}", original_chars);
    println!("  Optimized chars: {}", optimized_chars);
    println!("  Cache hits:      {}", tx.file_cache_hits);

    // Content changed, so no cache hit
    assert_eq!(tx.file_cache_hits, 0, "Modified file should NOT be cached");

    // Verify both reads have their full content
    let first_read = result.request.messages.iter()
        .find(|m| m.tool_call_id.as_deref() == Some("c1")).unwrap();
    let second_read = result.request.messages.iter()
        .find(|m| m.tool_call_id.as_deref() == Some("c3")).unwrap();

    assert!(first_read.content.as_ref().unwrap().as_text().contains("Flask"),
        "First read should have original content");
    assert!(second_read.content.as_ref().unwrap().as_text().contains("FIXED"),
        "Second read should have modified content");
}

/// Scenario 10: Different tool names (Read, read_file, cat, view_file)
/// Pattern: Various agent frameworks use different names for file reading
#[test]
fn scenario_different_tool_names() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("Read"), tool("read_file"), tool("cat")];

    let messages = vec![
        sys("Assistant"),
        user("Read the go file"),
        // Using "Read" tool name (Claude Code style)
        asst_read_with_name("c1", "/src/audit_target.go", "Read"),
        tool_result("c1", AUDIT_TARGET_GO),
        asst("Now let me re-read with the same tool."),
        asst_read_with_name("c2", "/src/audit_target.go", "Read"),
        tool_result("c2", AUDIT_TARGET_GO),
    ];

    let result = tx.transform(req(tools, messages));

    println!("\n[Scenario 10: Different Tool Names]");
    println!("  Cache hits: {}", tx.file_cache_hits);
    println!("  Est tokens saved: {}", result.estimated_tokens_saved);

    assert_eq!(tx.file_cache_hits, 1, "Should recognize 'Read' as a file read tool");
}

/// Scenario 11: Large conversation with 3x reads of the same file
/// Pattern: Long debugging session reading the same file 3 times
/// Expected: 2nd and 3rd reads get cached
#[test]
fn scenario_long_debug_session() {
    let mut tx = RequestTransformer::new();
    let tools = vec![tool("ReadFile"), tool("Bash"), tool("Grep")];

    let messages = vec![
        sys("You are a debugging assistant."),
        user("I'm getting a 500 error. Help me debug."),
        // Read 1
        asst_read("c1", "/src/server.py"),
        tool_result("c1", SERVER_PY),
        asst("I see. Let me check the logs."),
        user("Here's the error: TypeError at line 45"),
        // Read 2 (to look at line 45)
        asst_read("c2", "/src/server.py"),
        tool_result("c2", SERVER_PY),
        asst("The issue is in the verify_password function. Let me also check the Go file for comparison."),
        asst_read("c3", "/src/audit_target.go"),
        tool_result("c3", AUDIT_TARGET_GO),
        user("Actually, can you re-check the Python file one more time?"),
        // Read 3 of server.py
        asst_read("c4", "/src/server.py"),
        tool_result("c4", SERVER_PY),
    ];

    let original_chars = count_chars(&req(tools.clone(), messages.clone()));
    let result = tx.transform(req(tools, messages));
    let optimized_chars = count_chars(&result.request);
    let saved = original_chars - optimized_chars;

    println!("\n[Scenario 11: Long Debug Session]");
    println!("  Original chars:  {}", original_chars);
    println!("  Optimized chars: {}", optimized_chars);
    println!("  Chars saved:     {} ({:.1}%)", saved, (saved as f64 / original_chars as f64) * 100.0);
    println!("  Est tokens saved: {}", result.estimated_tokens_saved);
    println!("  Cache hits:      {}", tx.file_cache_hits);

    // server.py read 3x: 2nd and 3rd are cache hits
    assert_eq!(tx.file_cache_hits, 2, "2nd and 3rd reads of server.py should be cache hits");
    assert!(saved > 4000, "Should save >4000 chars from two duplicate reads");
}

/// Summary test: print overall statistics across all scenarios
#[test]
fn zzz_summary() {
    println!("\n{}", "=".repeat(60));
    println!("FILE READ CACHING VALIDATION SUMMARY");
    println!("{}", "=".repeat(60));

    struct ScenarioResult {
        name: &'static str,
        original: usize,
        optimized: usize,
        cache_hits: usize,
        tokens_saved: i64,
    }

    let mut results = Vec::new();

    // Run all scenarios in sequence and collect results
    let scenarios: Vec<(&str, Vec<Tool>, Vec<Message>)> = vec![
        ("Backend Dev (1 file, 2 reads)",
         vec![tool("ReadFile")],
         vec![
            sys("Assistant"), user("Fix bug"),
            asst_read("c1", "/f.py"), tool_result("c1", SERVER_PY),
            asst("Re-reading..."),
            asst_read("c2", "/f.py"), tool_result("c2", SERVER_PY),
         ]),
        ("Security Audit (2 files, 4 reads)",
         vec![tool("ReadFile")],
         vec![
            sys("Assistant"), user("Audit"),
            asst_read("c1", "/a.go"), tool_result("c1", AUDIT_TARGET_GO),
            asst_read("c2", "/b.py"), tool_result("c2", SERVER_PY),
            asst("Re-reading both..."),
            asst_read("c3", "/a.go"), tool_result("c3", AUDIT_TARGET_GO),
            asst_read("c4", "/b.py"), tool_result("c4", SERVER_PY),
         ]),
        ("ML Training (2 files, 4 reads)",
         vec![tool("ReadFile")],
         vec![
            sys("ML"), user("Review"),
            asst_read("c1", "/train.py"), tool_result("c1", TRAIN_PY),
            asst_read("c2", "/cfg.json"), tool_result("c2", r#"{"n_samples":1000,"n_features":10,"lr":0.01,"epochs":200}"#),
            asst("Re-reading..."),
            asst_read("c3", "/train.py"), tool_result("c3", TRAIN_PY),
            asst_read("c4", "/cfg.json"), tool_result("c4", r#"{"n_samples":1000,"n_features":10,"lr":0.01,"epochs":200}"#),
         ]),
        ("DevOps (2 configs, 4 reads)",
         vec![tool("ReadFile")],
         vec![
            sys("DevOps"), user("Review"),
            asst_read("c1", "/nginx.conf"), tool_result("c1", NGINX_CONF),
            asst_read("c2", "/Dockerfile"), tool_result("c2", DOCKERFILE),
            asst("Re-reading..."),
            asst_read("c3", "/nginx.conf"), tool_result("c3", NGINX_CONF),
            asst_read("c4", "/Dockerfile"), tool_result("c4", DOCKERFILE),
         ]),
        ("Teacher (1 CSV, 3 reads)",
         vec![tool("ReadFile")],
         vec![
            sys("Teacher"), user("Analyze"),
            asst_read("c1", "/data.csv"), tool_result("c1", STUDENTS_CSV),
            asst("Re-reading..."),
            asst_read("c2", "/data.csv"), tool_result("c2", STUDENTS_CSV),
            asst("Once more..."),
            asst_read("c3", "/data.csv"), tool_result("c3", STUDENTS_CSV),
         ]),
        ("Product Manager (1 PRD, 2 reads)",
         vec![tool("ReadFile")],
         vec![
            sys("PM"), user("Review PRD"),
            asst_read("c1", "/prd.md"), tool_result("c1", PRD_MD),
            asst("Re-reading..."),
            asst_read("c2", "/prd.md"), tool_result("c2", PRD_MD),
         ]),
        ("Long Debug (1 file, 3 reads + 1 other)",
         vec![tool("ReadFile")],
         vec![
            sys("Debug"), user("Help"),
            asst_read("c1", "/s.py"), tool_result("c1", SERVER_PY),
            asst("Checking..."),
            asst_read("c2", "/s.py"), tool_result("c2", SERVER_PY),
            asst_read("c3", "/o.go"), tool_result("c3", AUDIT_TARGET_GO),
            user("Check again"),
            asst_read("c4", "/s.py"), tool_result("c4", SERVER_PY),
         ]),
        ("Safety: Cross-request (NO cache hit expected)",
         vec![tool("ReadFile")],
         vec![
            sys("Safe"), user("Read"),
            asst_read("c1", "/f.py"), tool_result("c1", SERVER_PY),
         ]),
    ];

    let mut total_original = 0usize;
    let mut total_optimized = 0usize;
    let mut total_cache_hits = 0usize;
    let mut total_tokens_saved = 0i64;

    for (name, tools, messages) in scenarios {
        let mut tx = RequestTransformer::new();
        let original = count_chars(&req(tools.clone(), messages.clone()));
        let result = tx.transform(req(tools, messages));
        let optimized = count_chars(&result.request);

        total_original += original;
        total_optimized += optimized;
        total_cache_hits += tx.file_cache_hits;
        total_tokens_saved += result.estimated_tokens_saved;

        results.push(ScenarioResult {
            name,
            original,
            optimized,
            cache_hits: tx.file_cache_hits,
            tokens_saved: result.estimated_tokens_saved,
        });
    }

    println!("\n{:-<70}", "");
    println!("{:<45} {:>6} {:>6} {:>5} {:>6}", "Scenario", "Before", "After", "Hits", "Saved");
    println!("{:-<70}", "");
    for r in &results {
        let pct = if r.original > 0 {
            ((r.original - r.optimized) as f64 / r.original as f64) * 100.0
        } else { 0.0 };
        println!("{:<45} {:>6} {:>6} {:>5} {:>5.1}%",
            r.name, r.original, r.optimized, r.cache_hits, pct);
    }
    println!("{:-<70}", "");
    let total_pct = if total_original > 0 {
        ((total_original - total_optimized) as f64 / total_original as f64) * 100.0
    } else { 0.0 };
    println!("{:<45} {:>6} {:>6} {:>5} {:>5.1}%",
        "TOTAL", total_original, total_optimized, total_cache_hits, total_pct);
    println!("\nTotal estimated tokens saved: {}", total_tokens_saved);
    println!("Total cache hits: {}", total_cache_hits);

    // Aggregate assertions
    assert!(total_cache_hits >= 8, "Should have at least 8 cache hits across all scenarios");
    assert!(total_tokens_saved > 2000, "Should save >2000 tokens total");
    assert!(total_pct > 15.0, "Should save >15% of chars overall");
}
