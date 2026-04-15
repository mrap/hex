FROM python:3.10-slim

RUN apt-get update && apt-get install -y --no-install-recommends git && \
    rm -rf /var/lib/apt/lists/*

# Git needs a user for commits (not used, but prevents warnings)
RUN git config --global user.email "test@test.com" && \
    git config --global user.name "Test"

COPY . /tmp/hex-setup
COPY tests/test_e2e.sh /test_e2e.sh
RUN chmod +x /test_e2e.sh

CMD ["/test_e2e.sh"]
