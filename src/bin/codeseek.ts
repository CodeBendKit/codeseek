#!/usr/bin/env node

/**
 * CodeSeek CLI Entry Point
 *
 * Flow:
 * 1. Check ~/.codeseek/config.json exists
 *    -> No: Run interactive setup wizard
 * 2. Check ~/.codeseek/bin/codeseek exists
 *    -> No: Download platform binary
 * 3. Pass-through args to Rust binary
 */

import * as fs from "fs";
import * as path from "path";
import * as os from "os";
import { spawnSync } from "child_process";
import { downloadBinary } from "../install/download";

const HOME = os.homedir();
const CODESEEK_DIR = path.join(HOME, ".codeseek");
const CONFIG_PATH = path.join(CODESEEK_DIR, "config.json");
const BIN_DIR = path.join(CODESEEK_DIR, "bin");
const BIN_PATH = path.join(BIN_DIR, "codeseek");

function ensureDir(dir: string): void {
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }
}

function configExists(): boolean {
  return fs.existsSync(CONFIG_PATH);
}

function binaryExists(): boolean {
  return fs.existsSync(BIN_PATH);
}

/**
 * Simple interactive setup wizard.
 * Uses readline for compatibility (no @clack/prompts dependency needed for basic use).
 */
async function runSetupWizard(): Promise<void> {
  const readline = require("readline");
  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout,
  });

  const question = (q: string): Promise<string> =>
    new Promise((resolve) => rl.question(q, resolve));

  console.log("\n  Welcome to CodeSeek!");
  console.log("  Let's configure the embedding model for semantic search.\n");

  console.log("  Provider: openai-compatible (default)");
  const apiBaseUrl =
    (await question("  API Base URL [https://api.siliconflow.cn/v1]: ")) ||
    "https://api.siliconflow.cn/v1";

  const model =
    (await question("  Model [Qwen/Qwen3-Embedding-4B]: ")) ||
    "Qwen/Qwen3-Embedding-4B";

  const apiToken = await question("  API Token: ");
  if (!apiToken) {
    console.log("\n  API Token is required. Aborting.");
    rl.close();
    process.exit(1);
  }

  const config = {
    embedding: {
      provider: "openai-compatible",
      model,
      api_token: apiToken,
      api_base_url: apiBaseUrl,
      dimensions: 2560,
    },
    index: {
      min_code_block_length: 16,
      enable_reranker: false,
      hybrid: {
        enable_bm25: true,
        bm25_top_k: 100,
        vector_top_k: 100,
        rrf_k: 60.0,
        rrf_top_k: 20,
        short_code_threshold: 30,
        short_code_penalty: 0.5,
      },
    },
    installed_hooks: {},
  };

  ensureDir(path.dirname(CONFIG_PATH));
  fs.writeFileSync(CONFIG_PATH, JSON.stringify(config, null, 2));
  console.log(`\n  Configuration saved to ${CONFIG_PATH}\n`);

  rl.close();
}

async function main(): Promise<void> {
  // Step 1: Check config
  if (!configExists()) {
    console.log("First time setup — configuring CodeSeek...");
    await runSetupWizard();
  }

  // Step 2: Check binary
  if (!binaryExists()) {
    console.log("Downloading CodeSeek binary...");
    ensureDir(BIN_DIR);
    try {
      await downloadBinary(BIN_PATH);
    } catch (err: any) {
      console.error(`Failed to download binary: ${err.message}`);
      console.error("Please install manually or use: brew install codeseek");
      process.exit(1);
    }
  }

  // Step 3: Make executable
  try {
    fs.chmodSync(BIN_PATH, 0o755);
  } catch {
    // ignore
  }

  // Step 4: Pass through to Rust binary
  const args = process.argv.slice(2);
  const result = spawnSync(BIN_PATH, args, {
    stdio: "inherit",
    env: process.env,
  });

  if (result.error) {
    console.error(`Failed to run codeseek: ${result.error.message}`);
    process.exit(1);
  }

  process.exit(result.status ?? 0);
}

main().catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});
