# Contributing Guide

## Getting Started

We use [Nix](https://nixos.org/) to manage development dependencies.

### 1. Install Nix

Follow the official guide:
👉 https://nixos.org/download.html

For most systems:
```bash
curl -L https://nixos.org/nix/install | sh
```
Then restart your shell or run
```bash
source `~/.nix-profile/etc/profile.d/nix.sh`.
```

To enable modern Nix features, it's recommended to add the following line to your `~/.config/nix/nix.conf`:

```
experimental-features = nix-command flakes
```

### 2. (Optional) Install direnv
If you'd like your development environment to load automatically when entering the project directory, you can install and configure `direnv`.

Installation and shell integration instructions:
👉 https://direnv.net/docs/installation.html

After setting it up, enable it in the project directory with:
```bash
direnv allow
```

### 3. (Alternative) Manually Start the Nix Shell
If you prefer not to use `direnv`, you can manually enter the development environment with:
```bash
nix develop
```

This will load all dependencies and environment settings defined in `flake.nix`.
