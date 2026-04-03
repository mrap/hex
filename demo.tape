# hex demo — persistent memory for AI coding agents
# Records a GIF showing setup, save, and search

Output assets/demo.gif

Set Shell "bash"
Set FontSize 14
Set Width 900
Set Height 500
Set Theme "Dracula"
Set TypingSpeed 40ms
Set Padding 20

# Clean start
Hide
Type "cd /tmp && rm -rf hex-demo && mkdir hex-demo && cd hex-demo && git init -q"
Enter
Type "cp -r ~/github.com/mrap/hex-hermes/{CLAUDE.md,AGENTS.md,setup.sh,.hex} ."
Enter
Type "rm -f .hex/memory/memory.db"
Enter
Type "clear"
Enter
Show

# Step 1: Setup
Sleep 500ms
Type "# Step 1: Set up hex (30 seconds, zero dependencies)"
Sleep 1s
Enter
Sleep 500ms
Type "bash setup.sh"
Enter
Sleep 2s

# Step 2: Save memories
Type "# Step 2: Save what your agent learns"
Sleep 1s
Enter
Sleep 500ms
Type "python3 .hex/memory/save.py 'Project uses Express with JWT auth, refresh tokens in httpOnly cookies' --tags 'auth,architecture'"
Enter
Sleep 1.5s

Type "python3 .hex/memory/save.py 'Database is PostgreSQL 15, migrations via Prisma, connection pooling with PgBouncer' --tags 'database,infra'"
Enter
Sleep 1.5s

Type "python3 .hex/memory/save.py 'Deploy via GitHub Actions → AWS ECS Fargate, staging auto-deploys on PR merge' --tags 'deploy,ci'"
Enter
Sleep 1.5s

# Step 3: Search
Type "# Step 3: Your agent searches before guessing"
Sleep 1s
Enter
Sleep 500ms
Type "python3 .hex/memory/search.py 'authentication' --compact"
Enter
Sleep 2s

Type "python3 .hex/memory/search.py 'database' --compact"
Enter
Sleep 2s

# Done
Type "# ✅ Your agent now remembers across sessions."
Sleep 1s
Enter
Type "# No API keys. No cloud. No dependencies. Just SQLite."
Enter
Sleep 3s
