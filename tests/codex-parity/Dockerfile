FROM python:3.11-slim

# Install Node.js for codex CLI
RUN apt-get update && apt-get install -y \
    curl \
    git \
    bash \
    ca-certificates \
    gnupg \
    && curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y nodejs \
    && apt-get clean && rm -rf /var/lib/apt/lists/*

# Install codex CLI
RUN npm install -g @openai/codex

# Set working directory
WORKDIR /hex-src

# Copy the repo into the image (build context is the hex-foundation root)
COPY . /hex-src/

# Install hex into /tmp/hex-install
RUN bash /hex-src/install.sh /tmp/hex-install 2>&1 || true

# Pass OPENAI_API_KEY at runtime — skips live tests if absent
ENV OPENAI_API_KEY=""

# Run the parity suite
CMD ["bash", "/hex-src/tests/codex-parity/run-all.sh"]
