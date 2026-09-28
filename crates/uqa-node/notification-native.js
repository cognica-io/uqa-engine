//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { subscribeDirect, NotificationSubscription } = require("./notification-direct.js");

function installNotifications(Engine) {
  Object.defineProperty(Engine.prototype, "subscribeNotifications", {
    configurable: true, writable: true,
    value: function subscribeNotifications(channels, options) {
      return subscribeDirect(channels, options, (names, limits) => this._notificationHandle(names, limits));
    },
  });
}

module.exports = { installNotifications, NotificationSubscription };
