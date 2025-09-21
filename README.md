# Portfolio Tracker

A command-line tool for tracking stock portfolio holdings by processing transaction reports from brokers.

---

## About The Project

This tool provides a way to get a clear overview of your stock portfolio. It processes raw transaction data from brokers, calculates your current holdings, and displays a summarized view. It is designed to be simple, fast, and extensible for different brokers and data formats.

## Features

*   **Broker Integration**: Currently supports processing transaction reports from Interactive Brokers (`ib_combined.csv`).
*   **Transaction Consolidation**: Merges transaction data into a single, clean CSV file (`transactions.csv`).
*   **Holdings Calculation**: Calculates the quantity, total cost, and average cost for each security you hold.
*   **Portfolio Summary**: Displays a clear, table-based summary of your holdings, including percentages of your total portfolio.
*   **Duplicate Handling**: Detects and skips duplicate transactions to ensure data accuracy.
*   **Flexible Display**: Allows sorting holdings by name or percentage, in ascending or descending order.

## Getting Started

### Prerequisites

While you can install dependencies manually, it is **recommended** to use [Nix](https://nixos.org/) to manage your development environment and dependencies.
Please see [`docs/setup.md`](docs/setup.md) for instructions on installing and configuring Nix.

All required dependencies are specified in the `flake.nix` configuration for reproducible builds.

### Installation

1.  Clone the repository.
2.  Enter the project directory and start the Nix development environment:
    ```sh
    nix develop
    ```
3.  Build the project:
    ```sh
    cargo build --release
    ```

## Usage

This tool has two main commands: `init` and the default calculation/display command.

### Initialize from Broker Data

To process your broker's transaction report for the first time or to incorporate new transactions, use the `init` command.

Place your Interactive Brokers report file (e.g., `ib_combined.csv`) in the root of the project directory. Then run:

```sh
cargo run -- init
```

This will:
1.  Read the data from `ib_combined.csv`.
2.  Process and clean the data.
3.  Create a consolidated `transactions.csv` file.
4.  Display the calculated portfolio holdings.

You can specify different input and output files:
```sh
cargo run -- init --ib-file <path/to/your/ib_report.csv> --transactions-file <path/to/your/output.csv>
```

### View Your Portfolio

Once you have a `transactions.csv` file, you can view your portfolio at any time by running the tool without any subcommands:

```sh
cargo run
```

This will read `transactions.csv` and display your holdings.

You can customize the output with the following options:

*   `--sort-by <name|percentage>`: Sort the holdings by ticker name or by their percentage of the portfolio. (Default: `percentage`)
*   `--order <+|->`: Sort in ascending (`+`) or descending (`-`) order. (Default: `-`)

**Example:** Sort holdings by name in ascending order.

```sh
cargo run -- --sort-by name --order +
```