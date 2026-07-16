// Text imports: wrangler's `[[rules]] type = "Text"` (see wrangler.toml) bundles
// `*.sh` files as their string contents. This declares the shape for TS so
// `import script from "../../install.sh"` type-checks.
declare module "*.sh" {
  const content: string;
  export default content;
}
