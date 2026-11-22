use super::*;
use w11term::terminal::ScrollbackView;

fn invalid_operation() -> Error {
    Error::from_hresult(HRESULT(UIA_E_INVALIDOPERATION as i32))
}

#[derive(PartialEq)]
struct Metrics {
    scrollable: bool,
    percent: f64,
    size: f64,
}

impl Metrics {
    fn from_view(view: Option<ScrollbackView>) -> Self {
        if let Some(view) = view {
            let history = (view.live - view.oldest) as f64;
            let rows = view.rows as f64;
            Self {
                scrollable: true,
                percent: 100.0 * (view.top - view.oldest) as f64 / history,
                size: 100.0 * rows / (history + rows),
            }
        } else {
            Self {
                scrollable: false,
                percent: UIA_ScrollPatternNoScroll,
                size: 100.0,
            }
        }
    }
}

pub(super) fn notify(provider: &IRawElementProviderSimple, before: &Snapshot, after: &Snapshot) {
    let old = Metrics::from_view(before.scrollback_view());
    let new = Metrics::from_view(after.scrollback_view());
    fn changed<T: PartialEq + Into<VARIANT>>(
        provider: &IRawElementProviderSimple,
        id: UIA_PROPERTY_ID,
        old: T,
        new: T,
    ) {
        if old != new {
            unsafe {
                let _ =
                    UiaRaiseAutomationPropertyChangedEvent(provider, id, &old.into(), &new.into());
            }
        }
    }
    changed(
        provider,
        UIA_ScrollVerticallyScrollablePropertyId,
        old.scrollable,
        new.scrollable,
    );
    changed(
        provider,
        UIA_ScrollVerticalScrollPercentPropertyId,
        old.percent,
        new.percent,
    );
    changed(
        provider,
        UIA_ScrollVerticalViewSizePropertyId,
        old.size,
        new.size,
    );
}

impl Provider_Impl {
    fn scroll_view(&self) -> Result<Option<ScrollbackView>> {
        self.source.check()?;
        Ok(self.source.frames.load().scrollback_view())
    }

    fn apply_viewport(&self, view: ScrollbackView, top: isize) -> Result<()> {
        let target = view.target((top < view.live).then_some(top.max(view.oldest)));
        self.source
            .apply_scroll(RangeScroll::viewport(target, self.source.alive.clone()))
    }
}

impl IScrollProvider_Impl for Provider_Impl {
    fn Scroll(&self, horizontal: ScrollAmount, vertical: ScrollAmount) -> Result<()> {
        let view = self.scroll_view()?;
        let step = |amount| match amount {
            ScrollAmount_NoAmount => Ok(0),
            ScrollAmount_SmallDecrement => Ok(-1),
            ScrollAmount_SmallIncrement => Ok(1),
            ScrollAmount_LargeDecrement => Ok(-(view.map_or(0, |v| v.rows) as isize)),
            ScrollAmount_LargeIncrement => Ok(view.map_or(0, |v| v.rows) as isize),
            _ => Err(Error::from_hresult(E_INVALIDARG)),
        };
        step(horizontal)?;
        let step = step(vertical)?;
        if horizontal != ScrollAmount_NoAmount {
            return Err(invalid_operation());
        }
        if vertical == ScrollAmount_NoAmount {
            return Ok(());
        }
        let view = view.ok_or_else(invalid_operation)?;
        self.source
            .apply_scroll(RangeScroll::relative(view, step, self.source.alive.clone()))
    }

    fn SetScrollPercent(&self, horizontal: f64, vertical: f64) -> Result<()> {
        let view = self.scroll_view()?;
        let valid = |v| v == UIA_ScrollPatternNoScroll || (0.0..=100.0).contains(&v);
        if !valid(horizontal) || !valid(vertical) {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        if horizontal != UIA_ScrollPatternNoScroll {
            return Err(invalid_operation());
        }
        if vertical == UIA_ScrollPatternNoScroll {
            return Ok(());
        }
        let view = view.ok_or_else(invalid_operation)?;
        let top =
            view.oldest + ((view.live - view.oldest) as f64 * (vertical / 100.0)).round() as isize;
        self.apply_viewport(view, top)
    }

    fn HorizontalScrollPercent(&self) -> Result<f64> {
        self.source.check()?;
        Ok(UIA_ScrollPatternNoScroll)
    }
    fn VerticalScrollPercent(&self) -> Result<f64> {
        Ok(Metrics::from_view(self.scroll_view()?).percent)
    }
    fn HorizontalViewSize(&self) -> Result<f64> {
        self.source.check()?;
        Ok(100.0)
    }
    fn VerticalViewSize(&self) -> Result<f64> {
        Ok(Metrics::from_view(self.scroll_view()?).size)
    }
    fn HorizontallyScrollable(&self) -> Result<BOOL> {
        self.source.check()?;
        Ok(false.into())
    }
    fn VerticallyScrollable(&self) -> Result<BOOL> {
        Ok(self.scroll_view()?.is_some().into())
    }
}
