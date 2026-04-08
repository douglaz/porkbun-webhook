{
  description = "Porkbun DNS webhook provider for external-dns";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
          targets = [ "x86_64-unknown-linux-musl" ];
        };

        # Build the porkbun-webhook binary
        porkbun-webhook = pkgs.rustPlatform.buildRustPackage {
          pname = "porkbun-webhook";
          version = "0.1.0";
          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = with pkgs; [
            pkg-config
            rustToolchain
          ];

          buildInputs = with pkgs; [
          ] ++ lib.optionals stdenv.isDarwin [
            darwin.apple_sdk.frameworks.Security
            darwin.apple_sdk.frameworks.SystemConfiguration
          ];

          # Build for musl target for static linking
          CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgs.pkgsStatic.stdenv.cc}/bin/${pkgs.pkgsStatic.stdenv.cc.targetPrefix}cc";

          # Skip tests during build (can run separately)
          doCheck = false;

          meta = with pkgs.lib; {
            description = "Porkbun DNS webhook provider for external-dns";
            homepage = "https://github.com/douglaz/porkbun-webhook";
            license = licenses.mit;
          };
        };
      in
      {
        # Package outputs
        packages = {
          default = porkbun-webhook;

          # Docker image
          dockerImage = pkgs.dockerTools.buildImage {
            name = "porkbun-webhook";
            tag = "latest";

            copyToRoot = pkgs.buildEnv {
              name = "image-root";
              paths = [
                porkbun-webhook
                pkgs.cacert
                pkgs.wget  # used by healthcheck
              ];
              pathsToLink = [ "/bin" "/etc" "/share" ];
            };

            config = {
              Cmd = [ "/bin/porkbun-webhook" ];
              ExposedPorts = {
                "8888/tcp" = {};
              };
              Env = [
                "RUST_LOG=info"
                "WEBHOOK_HOST=0.0.0.0"
                "WEBHOOK_PORT=8888"
                "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                "SYSTEM_CERTIFICATE_PATH=${pkgs.cacert}/etc/ssl/certs"
                "PATH=/bin:/usr/bin:/usr/local/bin"
              ];
              Labels = {
                "org.opencontainers.image.source" = "https://github.com/douglaz/porkbun-webhook";
                "org.opencontainers.image.description" = "Porkbun DNS webhook provider for external-dns";
                "org.opencontainers.image.licenses" = "MIT";
              };
              Healthcheck = {
                Test = ["CMD" "/bin/wget" "--no-verbose" "--tries=1" "--spider" "http://localhost:8888/healthz"];
                Interval = 30000000000; # 30 seconds in nanoseconds
                Timeout = 5000000000;   # 5 seconds in nanoseconds
                Retries = 3;
              };
            };
          };
        };

        # Development shell
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustToolchain
            rust-analyzer
            cargo-watch
            cargo-edit
            cargo-audit
            cargo-outdated
            pkg-config

            # Development tools
            just
            bacon
            tokio-console

            # For API testing
            curl
            jq
            httpie
          ];

          RUST_BACKTRACE = "full";
          RUST_LOG = "debug";

          shellHook = ''
            echo "Porkbun Webhook Development Environment"
            echo ""
            echo "Available commands:"
            echo "  cargo build           - Build the project"
            echo "  cargo run             - Run the webhook server"
            echo "  cargo test            - Run tests"
            echo "  cargo watch -x run    - Auto-rebuild on changes"
            echo "  bacon                 - Background rust compiler"
            echo ""
            echo "Build Docker image:"
            echo "  nix build .#dockerImage"
            echo "  docker load < result"
            echo ""

            # Create .env from example if it doesn't exist
            if [ ! -f .env ] && [ -f .env.example ]; then
              cp .env.example .env
              echo "Created .env from .env.example - please configure your Porkbun API credentials"
            fi
          '';
        };

        # Apps for nix run
        apps.default = flake-utils.lib.mkApp {
          drv = porkbun-webhook;
        };

        # Checks for CI
        checks = {
          inherit porkbun-webhook;

          # Format check
          format = pkgs.runCommand "format-check" {} ''
            cd ${./.}
            ${rustToolchain}/bin/cargo fmt --check
            touch $out
          '';

          # Clippy check
          clippy = pkgs.runCommand "clippy-check" {} ''
            cd ${./.}
            ${rustToolchain}/bin/cargo clippy --all-targets --all-features -- -D warnings
            touch $out
          '';
        };
      }
    );
}
