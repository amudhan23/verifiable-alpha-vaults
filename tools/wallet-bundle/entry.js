// Bundled once into frontend/vendor/wallet.js so the site stays no-build.
// Rebuild: cd tools/wallet-bundle && npm i && node build.mjs
import { setupWalletSelector } from "@near-wallet-selector/core";
import { setupModal } from "@near-wallet-selector/modal-ui";
import { setupMyNearWallet } from "@near-wallet-selector/my-near-wallet";
import { setupMeteorWallet } from "@near-wallet-selector/meteor-wallet";
import { setupIntearWallet } from "@near-wallet-selector/intear-wallet";

export async function initWallet(contractId) {
  const selector = await setupWalletSelector({
    network: "testnet",
    modules: [setupMeteorWallet(), setupMyNearWallet(), setupIntearWallet()],
  });
  const modal = setupModal(selector, { contractId });
  return { selector, modal };
}
