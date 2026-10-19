# Build from the repository root:
#   docker build -f forked-server/Dockerfile -t forked-server .
# Run:
#   docker run -p 8080:8080 -v forked-data:/data forked-server
FROM rust:1-slim AS build
WORKDIR /app
COPY . .
RUN cargo build --release -p forked-server

FROM debian:bookworm-slim
COPY --from=build /app/target/release/forked-server /usr/local/bin/forked-server
ENV FORKED_ADDR=0.0.0.0:8080 \
    FORKED_DB=/data/forked.db
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["forked-server"]
