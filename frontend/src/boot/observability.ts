import { defineBoot } from "#q-app/wrappers";
import {
  captureFrontendError,
  installFrontendErrorReporter,
} from "src/observability/error-reporter";

let disposeActiveReporter: (() => void) | undefined;

export default defineBoot(({ app, router }) => {
  disposeActiveReporter?.();
  disposeActiveReporter = installFrontendErrorReporter(() =>
    String(router.currentRoute.value.name ?? "unknown"),
  );

  const previousErrorHandler = app.config.errorHandler;
  app.config.errorHandler = (cause, instance, info) => {
    captureFrontendError(cause, { kind: "vue" });
    if (previousErrorHandler) {
      previousErrorHandler(cause, instance, info);
      return;
    }
    console.error("Vue application error", cause);
  };

  if (import.meta.hot) {
    import.meta.hot.dispose(() => {
      disposeActiveReporter?.();
      disposeActiveReporter = undefined;
      app.config.errorHandler = previousErrorHandler;
    });
  }
});
