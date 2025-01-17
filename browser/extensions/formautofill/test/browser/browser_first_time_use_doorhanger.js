/* eslint-disable mozilla/no-arbitrary-setTimeout */
"use strict";

const FORM_URL = "http://mochi.test:8888/browser/browser/extensions/formautofill/test/browser/autocomplete_basic.html";
const FTU_PREF = "extensions.formautofill.firstTimeUse";
const ENABLED_PREF = "extensions.formautofill.addresses.enabled";

add_task(async function test_first_time_save() {
  let addresses = await getAddresses();
  is(addresses.length, 0, "No address in storage");
  await SpecialPowers.pushPrefEnv({
    "set": [
      [FTU_PREF, true],
      [ENABLED_PREF, true],
    ],
  });

  await BrowserTestUtils.withNewTab({gBrowser, url: FORM_URL},
    async function(browser) {
      let promiseShown = BrowserTestUtils.waitForEvent(PopupNotifications.panel,
                                                       "popupshown");
      let tabPromise = BrowserTestUtils.waitForNewTab(gBrowser, "about:preferences#privacy");
      await ContentTask.spawn(browser, null, async function() {
        let form = content.document.getElementById("form");
        form.querySelector("#organization").focus();
        form.querySelector("#organization").value = "Sesame Street";
        form.querySelector("#street-address").value = "123 Sesame Street";
        form.querySelector("#tel").value = "1-345-345-3456";

        // Wait 500ms before submission to make sure the input value applied
        await new Promise(resolve => setTimeout(resolve, 500));
        form.querySelector("input[type=submit]").click();
      });

      await promiseShown;
      // Open the panel via main button
      await clickDoorhangerButton(MAIN_BUTTON_INDEX);
      let tab = await tabPromise;
      ok(tab, "Privacy panel opened");
      await BrowserTestUtils.removeTab(tab);
    }
  );

  addresses = await getAddresses();
  is(addresses.length, 1, "Address saved");
  let ftuPref = SpecialPowers.getBoolPref(FTU_PREF);
  is(ftuPref, false, "First time use flag is false");
});

add_task(async function test_non_first_time_save() {
  let addresses = await getAddresses();
  let ftuPref = SpecialPowers.getBoolPref(FTU_PREF);
  is(ftuPref, false, "First time use flag is false");
  is(addresses.length, 1, "1 address in storage");

  await BrowserTestUtils.withNewTab({gBrowser, url: FORM_URL},
    async function(browser) {
      await ContentTask.spawn(browser, null, async function() {
        let form = content.document.getElementById("form");
        form.querySelector("#organization").focus();
        form.querySelector("#organization").value = "Mozilla";
        form.querySelector("#street-address").value = "331 E. Evelyn Avenue";
        form.querySelector("#tel").value = "1-650-903-0800";

        // Wait 500ms before submission to make sure the input value applied
        await new Promise(resolve => setTimeout(resolve, 500));
        form.querySelector("input[type=submit]").click();
      });

      await sleep(1000);
      is(PopupNotifications.panel.state, "closed", "Doorhanger is hidden");
    }
  );

  addresses = await getAddresses();
  is(addresses.length, 2, "Another address saved");
});
