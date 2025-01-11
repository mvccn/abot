#!/bin/bash
if [ ! -d "$HOME/.config/bin" ]; then
    mkdir -p "$HOME/.config/bin"
fi
cp target/release/abot "$HOME/.config/bin/"
chmod +x "$HOME/.config/bin/abot"
echo "Installation complete! Make sure $HOME/.config/bin is in your PATH"