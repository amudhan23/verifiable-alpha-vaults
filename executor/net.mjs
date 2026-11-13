// Network settings, chosen from the vault's account id exactly like the contract
// does (contracts/vault/src/lib.rs `net()`): a `.testnet` vault trades on Ethereum
// Sepolia, any mainnet vault trades on Base.
export const NETS = {
  testnet: {
    near: "testnet",
    nearRpc: "https://test.rpc.fastnear.com",
    mpc: "v1.signer-prod.testnet",
    outlayerApi: "https://testnet-api.outlayer.ai",
    evm: { name: "Ethereum Sepolia", chainId: 11155111, viemChain: "sepolia", rpc: "https://ethereum-sepolia-rpc.publicnode.com", explorer: "https://sepolia.etherscan.io" },
    uni: {
      router: "0x3bFA4769FB09eefC5a80d6E87c3B9C650f7Ae48E", // SwapRouter02
      quoter: "0xEd1f6473345F45b75F8179591dd5bA1888cf2FB3", // QuoterV2
      weth: "0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14",
      usdc: "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238",
    },
    // NEAR Intents has no testnet: price the equivalent mainnet route
    intentsUsdc: "nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near",
    wrap: "wrap.testnet",
  },
  mainnet: {
    near: "mainnet",
    nearRpc: "https://rpc.mainnet.fastnear.com",
    mpc: "v1.signer",
    outlayerApi: "https://api.outlayer.fastnear.com",
    evm: { name: "Base", chainId: 8453, viemChain: "base", rpc: "https://base-rpc.publicnode.com", explorer: "https://basescan.org" },
    uni: {
      // Uniswap v3 on Base: developers.uniswap.org/docs/protocols/v3/deployments/v3-base-deployments
      router: "0x2626664c2603336E57B271c5C0b26F421741e481",
      quoter: "0x3d4e44Eb1374240CE5F1B871ab261CD16335B76a",
      weth: "0x4200000000000000000000000000000000000006",
      usdc: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    },
    intentsUsdc: "nep141:base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near",
    wrap: "wrap.near",
  },
};

export function netFor(accountId = "") {
  return accountId.endsWith(".testnet") ? NETS.testnet : NETS.mainnet;
}
