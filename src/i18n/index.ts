import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import zh from "./locales/zh.json";
import en from "./locales/en.json";

function detectLocale(): string {
  // 1. Previously user-selected locale
  const saved = localStorage.getItem("smart-api-gateway-locale");
  if (saved === "zh" || saved === "en") return saved;
  // 2. Default to English
  return "en";
}

i18n.use(initReactI18next).init({
  resources: {
    zh: { translation: zh },
    en: { translation: en },
  },
  lng: detectLocale(),
  fallbackLng: "en",
  interpolation: {
    escapeValue: false,
  },
});

export default i18n;
