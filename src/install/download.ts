/**
 * CodeSeek Binary Downloader
 *
 * Downloads the appropriate platform binary from GitHub Releases.
 */

import * as https from "https";
import * as fs from "fs";
import * as os from "os";

const REPO_OWNER = "CodeBendKit";
const REPO_NAME = "codeseek";
const VERSION = "v0.1.10";

function getPlatformSuffix(): { suffix: string; exe: boolean } {
  const platform = os.platform();
  const arch = os.arch();

  if (platform === "darwin") {
    return { suffix: arch === "arm64" ? "darwin-arm64" : "darwin-x64", exe: false };
  }
  if (platform === "linux") {
    return { suffix: "linux-x64", exe: false };
  }
  if (platform === "win32") {
    return { suffix: "win32-x64", exe: true };
  }
  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}

function getDownloadUrl(): string {
  const { suffix, exe } = getPlatformSuffix();
  const ext = exe ? ".exe" : "";
  return `https://github.com/${REPO_OWNER}/${REPO_NAME}/releases/download/${VERSION}/codeseek-${suffix}${ext}`;
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
