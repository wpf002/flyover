// ESLint 10 only reads flat config, so this replaces .eslintrc.
// Rules live in packages/config so every workspace lints the same way.
import config from "@flyover/config/eslint";

export default config;
