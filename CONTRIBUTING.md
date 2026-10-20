# Участие в проекте

Issue и pull request'ы приветствуются.

1. Прочитайте [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) и [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).
2. Перед PR: `cargo test --workspace` и `node scripts/test-shim.mjs` должны проходить; для изменений рендерера,
   расширения или GUI — прогон `scripts/nested-test.sh` (не затрагивает рабочую сессию).
3. Делайте небольшие сфокусированные коммиты, в описании — что и зачем. Новые D-Bus методы документируйте в ARCHITECTURE.md.
4. В баг-репорте укажите версию GNOME Shell, GPU/драйвер, тип и id обоев и логи
   (`journalctl --user -u wpe-daemon`).
5. Не добавляйте в репозиторий обои из Workshop (чужие авторские права), секреты и личные пути.

Лицензия вклада — GPL-3.0-or-later.
