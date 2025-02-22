/**
 * The following code is modified based on
 * https://github.com/webpack/webpack/tree/4b4ca3bb53f36a5b8fc6bc1bd976ed7af161bd80/lib/stats
 *
 * MIT Licensed
 * Author Tobias Koppers @sokra
 * Copyright (c) JS Foundation and other contributors
 * https://github.com/webpack/webpack/blob/main/LICENSE
 */
import type * as binding from "@rspack/binding";
import { Compilation, FilterItemTypes } from ".";
import { StatsValue, StatsOptions } from "./config";
import type { StatsCompilation } from "./stats/statsFactoryUtils";

export class Stats {
	#inner: binding.JsStats;
	compilation: Compilation;

	constructor(compilation: Compilation) {
		this.#inner = compilation.__internal_getInner().getStats();
		this.compilation = compilation;
	}

	get hash() {
		return this.compilation.hash;
	}

	hasErrors() {
		return this.#inner.getErrors().length > 0;
	}

	hasWarnings() {
		return this.#inner.getWarnings().length > 0;
	}

	toJson(opts?: StatsValue, forToString?: boolean): StatsCompilation {
		const options = this.compilation.createStatsOptions(opts, {
			forToString
		});

		const statsFactory = this.compilation.createStatsFactory(options);

		// FIXME: This is a really ugly workaround for avoid panic for accessing previous compilation.
		// webpack-dev-server and Modern.js dev server will detect whether the returned stats is available.
		// So this does not do harm to these frameworks.
		// webpack-dev-server: https://github.com/webpack/webpack-dev-server/blob/540c43852ea33f9cb18820e1cef05d5ddb86cc3e/lib/Server.js#L3222
		// Modern.js: https://github.com/web-infra-dev/modern.js/blob/63f916f882f7d16096949e264e119218c0ab8d7d/packages/server/server/src/dev-tools/dev-middleware/socketServer.ts#L172
		let stats: StatsCompilation | null = null;
		try {
			stats = statsFactory.create("compilation", this.compilation, {
				compilation: this.compilation,
				_inner: this.#inner
			});
		} catch (e) {
			console.warn(
				"Failed to get stats. " +
					"Are you trying to access the stats from the previous compilation?"
			);
		}
		return stats as StatsCompilation;
	}

	toString(opts?: StatsValue) {
		const options = this.compilation.createStatsOptions(opts, {
			forToString: true
		});
		const statsFactory = this.compilation.createStatsFactory(options);

		const statsPrinter = this.compilation.createStatsPrinter(options);

		// FIXME: This is a really ugly workaround for avoid panic for accessing previous compilation.
		// webpack-dev-server and Modern.js dev server will detect whether the returned stats is available.
		// So this does not do harm to these frameworks.
		// webpack-dev-server: https://github.com/webpack/webpack-dev-server/blob/540c43852ea33f9cb18820e1cef05d5ddb86cc3e/lib/Server.js#L3222
		// Modern.js: https://github.com/web-infra-dev/modern.js/blob/63f916f882f7d16096949e264e119218c0ab8d7d/packages/server/server/src/dev-tools/dev-middleware/socketServer.ts#L172
		let stats: StatsCompilation | null = null;
		try {
			stats = statsFactory.create("compilation", this.compilation, {
				compilation: this.compilation,
				_inner: this.#inner
			});
		} catch (e) {
			console.warn(
				"Failed to get stats. " +
					"Are you trying to access the stats from the previous compilation?"
			);
		}

		if (!stats) {
			return "";
		}

		const result = statsPrinter.print("compilation", stats);

		return result === undefined ? "" : result;
	}
}

export function normalizeStatsPreset(options?: StatsValue): StatsOptions {
	if (typeof options === "boolean" || typeof options === "string")
		return presetToOptions(options);
	else if (!options) return {};
	else {
		let obj = { ...presetToOptions(options.preset), ...options };
		delete obj.preset;
		return obj;
	}
}

function presetToOptions(name?: boolean | string): StatsOptions {
	const pn = (typeof name === "string" && name.toLowerCase()) || name;
	switch (pn) {
		case "none":
			return {
				all: false
			};
		case "verbose":
			return {
				all: true
			};
		case "errors-only":
			return {
				all: false,
				errors: true,
				errorsCount: true,
				logging: "error"
				// TODO: moduleTrace: true,
			};
		case "errors-warnings":
			return {
				all: false,
				errors: true,
				errorsCount: true,
				warnings: true,
				warningsCount: true,
				logging: "warn"
			};
		default:
			return {};
	}
}

export const normalizeFilter = (item: FilterItemTypes) => {
	if (typeof item === "string") {
		const regExp = new RegExp(
			`[\\\\/]${item.replace(
				// eslint-disable-next-line no-useless-escape
				/[-[\]{}()*+?.\\^$|]/g,
				"\\$&"
			)}([\\\\/]|$|!|\\?)`
		);
		return (ident: string) => regExp.test(ident);
	}
	if (item && typeof item === "object" && typeof item.test === "function") {
		return (ident: string) => item.test(ident);
	}
	if (typeof item === "function") {
		return item;
	}
	if (typeof item === "boolean") {
		return () => item;
	}
	throw new Error(
		`unreachable: typeof ${item} should be one of string | RegExp | ((value: string) => boolean)`
	);
};

export const optionsOrFallback = (...args: any) => {
	let optionValues = [];
	optionValues.push(...args);
	return optionValues.find(optionValue => optionValue !== undefined);
};
