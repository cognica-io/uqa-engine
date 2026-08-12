//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

mergeInto(LibraryManager.library, {
  uqa_invoke_callback__deps: [
    "$UTF8ToString",
    "$lengthBytesUTF8",
    "$stringToUTF8",
    "malloc",
  ],
  uqa_invoke_callback: function(callbackId, requestPtr) {
    var response;
    try {
      if (typeof Module["uqaInvokeCallback"] !== "function") {
        throw new Error("JavaScript SQL callback bridge is not installed");
      }
      response = Module["uqaInvokeCallback"](callbackId, UTF8ToString(requestPtr));
      if (typeof response !== "string") {
        throw new Error("JavaScript SQL callback bridge must return JSON text");
      }
    } catch (error) {
      var message = error && error.message ? error.message : String(error);
      response = JSON.stringify({ error: message });
    }
    var length = lengthBytesUTF8(response) + 1;
    var result = _malloc(length);
    if (result === 0) {
      return 0;
    }
    stringToUTF8(response, result, length);
    return result;
  },

  uqa_free_callback_result__deps: ["free"],
  uqa_free_callback_result: function(ptr) {
    _free(ptr);
  },
});
