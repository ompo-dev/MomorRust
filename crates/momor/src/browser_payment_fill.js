function (expectedOrigin, card) {
    if (location.protocol !== 'https:' || location.origin !== expectedOrigin) {
        throw new Error('A pagina mudou. Abra novamente o cofre no site desejado.');
    }
    const values = {
        'cc-name': card.name,
        'cc-number': card.number,
        'cc-exp': String(card.month).padStart(2, '0') + '/' + card.year,
        'cc-exp-month': String(card.month).padStart(2, '0'),
        'cc-exp-year': String(card.year),
    };
    let filled = 0;
    const visit = (root, view) => {
        for (const field of root.querySelectorAll('input[autocomplete], select[autocomplete]')) {
            if (field.disabled || field.readOnly || !field.getClientRects().length) continue;
            const token = field.autocomplete.split(/\s+/).find(token => Object.hasOwn(values, token));
            if (!token) continue;
            let value = values[token];
            if (field.tagName === 'SELECT') {
                const match = Array.from(field.options).find(option => option.value === value ||
                    Number(option.value) === Number(value) ||
                    (token === 'cc-exp-year' && Number(option.value) === card.year % 100));
                if (!match) continue;
                value = match.value;
            }
            if (field.type === 'month' && token === 'cc-exp') value = card.year + '-' + values['cc-exp-month'];
            if (field.tagName === 'INPUT' && token === 'cc-exp-year' && field.maxLength === 2) {
                value = String(card.year % 100).padStart(2, '0');
            }
            const prototype = field.tagName === 'SELECT' ? view.HTMLSelectElement.prototype : view.HTMLInputElement.prototype;
            Object.getOwnPropertyDescriptor(prototype, 'value').set.call(field, value);
            field.dispatchEvent(new view.Event('input', { bubbles: true }));
            field.dispatchEvent(new view.Event('change', { bubbles: true }));
            filled++;
        }
        for (const element of root.querySelectorAll('*')) {
            if (element.shadowRoot) visit(element.shadowRoot, view);
        }
        for (const frame of root.querySelectorAll('iframe')) {
            try {
                if (frame.contentDocument) {
                    visit(frame.contentDocument, frame.contentWindow);
                }
            } catch (_) { /* Cross-origin payment frames require their provider's own UI. */ }
        }
    };
    visit(document, window);
    return filled;
}
