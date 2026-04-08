# Use pre-built binary
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates wget && \
    rm -rf /var/lib/apt/lists/*

COPY target/release/porkbun-webhook /usr/local/bin/porkbun-webhook
RUN chmod +x /usr/local/bin/porkbun-webhook

ENV WEBHOOK_HOST=0.0.0.0
ENV WEBHOOK_PORT=8888

EXPOSE 8888

CMD ["porkbun-webhook"]
