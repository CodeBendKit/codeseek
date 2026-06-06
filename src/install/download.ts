/**
 * CodeSeek Binary Downloader
 *
 * Downloads the appropriate platform binary from GitHub Releases.
 */

import * as https from "https";
import * as fs from "fs";
import * as path from "path";
import * as os from "os";

const REPO_OWNER = "wenwang";
const REPO_NAME = "codeseek";
const VERSION = "v0.1.0";

function getPlatformSuffix(): string {
  const platform = os.platform();
  const arch = os.arch();

  if (platform === "darwin") {
    return arch === "arm64" ? "darwin-arm64" : "darwin-x64";
  }
  if (platform === "linux") {
    return arch === "arm64" ? "linux-arm64" : "linux-x64";
  }
  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}

function getDownloadUrl(): string {
  const suffix = getPlatformSuffix();
  return `https://github.com/${REPO_OWNER}/${REPO_NAME}/releases/download/${VERSION}/codeseek-${suffix}`;
}

export function downloadBinary(destPath: string): Promise<void> {
  const url = getDownloadUrl();
  console.log(`  Downloading from ${url}`);

  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(destPath);
    const request = https.get(url, (response) => {
      // Follow redirects
      if (
        response.statusCode === 301 ||
        response.statusCode === 302 ||
        response.statusCode === 307
      ) {
        const redirectUrl = response.headers.location;
        if (!redirectUrl) {
          reject(new Error("Redirect with no location"));
          return;
        }
        https.get(redirectUrl, (redirectResponse) => {
          redirectResponse.pipe(file);
          file.on("finish", () => {
            file.close();
            resolve();
          });
        }).on("error", reject);
        return;
      }

      if (response.statusCode !== 200) {
        reject(
          new Error(
            `Download failed: HTTP ${response.statusCode}`
          )
        );
        return;
      }

      response.pipe(file);
      file.on("finish", () => {
        file.close();
        resolve();
      });
    });

    request.on("error", reject);
    file.on("error", reject);
  });
}
