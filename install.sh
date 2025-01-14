#!/bin/bash
if [ ! -d "$HOME/.cargo/bin" ]; then
    mkdir -p "$HOME/.cargo/bin"
fi
cp target/release/abot "$HOME/.cargo/bin/"
chmod +x "$HOME/.cargo/bin/abot"
echo "Installation complete! Make sure $HOME/.cargo/bin is in your PATH"