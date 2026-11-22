use super::*;
use windows::Win32::System::{
    Com::SAFEARRAY,
    Ole::*,
    Variant::{VARENUM, VT_R8, VT_UNKNOWN},
};

fn array(ty: VARENUM, count: usize) -> Result<*mut SAFEARRAY> {
    let raw = unsafe {
        SafeArrayCreateVector(
            ty,
            0,
            u32::try_from(count).map_err(|_| Error::from_hresult(E_INVALIDARG))?,
        )
    };
    if raw.is_null() {
        Err(Error::from_hresult(E_OUTOFMEMORY))
    } else {
        Ok(raw)
    }
}
fn put(raw: *mut SAFEARRAY, index: usize, pointer: *const std::ffi::c_void) -> Result<()> {
    if let Err(error) = unsafe { SafeArrayPutElement(raw, &(index as i32), pointer) } {
        unsafe {
            let _ = SafeArrayDestroy(raw);
        }
        return Err(error);
    }
    Ok(())
}
pub(super) fn interfaces(
    ranges: impl ExactSizeIterator<Item = ITextRangeProvider>,
) -> Result<*mut SAFEARRAY> {
    let raw = array(VT_UNKNOWN, ranges.len())?;
    for (index, range) in ranges.enumerate() {
        // For VT_UNKNOWN, SafeArrayPutElement accepts the interface itself and AddRefs it.
        put(raw, index, range.as_raw())?;
    }
    Ok(raw)
}

#[implement(ITextRangeProvider)]
pub(super) struct TextRange {
    owner: IRawElementProviderSimple,
    source: Arc<Source>,
    doc: Arc<Document>,
    span: Mutex<Range<usize>>,
}
impl TextRange {
    pub(super) fn create(
        owner: IRawElementProviderSimple,
        source: Arc<Source>,
        doc: Arc<Document>,
        span: Range<usize>,
    ) -> ITextRangeProvider {
        Self {
            owner,
            source,
            doc,
            span: Mutex::new(span),
        }
        .into()
    }
    fn span(&self) -> Result<Range<usize>> {
        self.source.check()?;
        Ok(self.span.lock().map_err(|_| unavailable())?.clone())
    }
    fn assign(&self, r: Range<usize>) -> Result<()> {
        self.source.check()?;
        *self.span.lock().map_err(|_| unavailable())? = r;
        Ok(())
    }
    fn with_span(&self, span: Range<usize>) -> ITextRangeProvider {
        Self::create(
            self.owner.clone(),
            self.source.clone(),
            self.doc.clone(),
            span,
        )
    }
    fn other(&self, target: Ref<ITextRangeProvider>) -> Result<Range<usize>> {
        let r = target.ok()?.cast_object_ref::<TextRange>()?;
        if !Arc::ptr_eq(&r.doc, &self.doc) || !Arc::ptr_eq(&r.source, &self.source) {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        r.span()
    }
}
fn endpoint(r: &Range<usize>, e: TextPatternRangeEndpoint) -> Result<usize> {
    match e {
        TextPatternRangeEndpoint_Start => Ok(r.start),
        TextPatternRangeEndpoint_End => Ok(r.end),
        _ => Err(Error::from_hresult(E_INVALIDARG)),
    }
}
fn set_endpoint(r: &mut Range<usize>, e: TextPatternRangeEndpoint, pos: usize) -> Result<()> {
    match e {
        TextPatternRangeEndpoint_Start => {
            r.start = pos;
            r.end = r.end.max(pos);
        }
        TextPatternRangeEndpoint_End => {
            r.end = pos;
            r.start = r.start.min(pos);
        }
        _ => return Err(Error::from_hresult(E_INVALIDARG)),
    }
    Ok(())
}
impl ITextRangeProvider_Impl for TextRange_Impl {
    fn Clone(&self) -> Result<ITextRangeProvider> {
        Ok(self.with_span(self.span()?))
    }
    fn Compare(&self, target: Ref<ITextRangeProvider>) -> Result<BOOL> {
        Ok(self
            .other(target)
            .is_ok_and(|s| self.span().is_ok_and(|own| own == s))
            .into())
    }
    fn CompareEndpoints(
        &self,
        e: TextPatternRangeEndpoint,
        target: Ref<ITextRangeProvider>,
        other: TextPatternRangeEndpoint,
    ) -> Result<i32> {
        Ok(endpoint(&self.span()?, e)? as i32 - endpoint(&self.other(target)?, other)? as i32)
    }
    fn ExpandToEnclosingUnit(&self, unit: TextUnit) -> Result<()> {
        self.assign(self.doc.unit(self.span()?.start, unit))
    }
    fn FindAttribute(
        &self,
        id: UIA_TEXTATTRIBUTE_ID,
        val: &VARIANT,
        _: BOOL,
    ) -> Result<ITextRangeProvider> {
        if id == UIA_IsReadOnlyAttributeId && bool::try_from(val).unwrap_or(false) {
            self.Clone()
        } else {
            Err(Error::empty())
        }
    }
    fn FindText(
        &self,
        text: &BSTR,
        backward: BOOL,
        ignorecase: BOOL,
    ) -> Result<ITextRangeProvider> {
        self.doc
            .find(
                self.span()?,
                &text.to_string(),
                ignorecase.as_bool(),
                backward.as_bool(),
            )
            .map(|r| self.with_span(r))
            .ok_or_else(Error::empty)
    }
    fn GetAttributeValue(&self, id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        self.source.check()?;
        if id == UIA_IsReadOnlyAttributeId {
            Ok(true.into())
        } else {
            Ok(unsafe { UiaGetReservedNotSupportedValue()? }.into())
        }
    }
    fn GetBoundingRectangles(&self) -> Result<*mut SAFEARRAY> {
        let span = self.span()?;
        let frame = self.source.frames.load();
        if span.is_empty()
            || !self
                .doc
                .retained_rows(&span)
                .is_some_and(|rows| rows.matches_visible(&frame))
        {
            return array(VT_R8, 0);
        }
        let (g, r) = self.source.rect()?;
        let mut values = Vec::new();
        for row in self.doc.covered_rows(&span) {
            let Some(visible) = self.doc.visible_row(row, &frame) else {
                continue;
            };
            let Some(&row_start) = self.doc.starts.get(row) else {
                break;
            };
            let end = self.doc.end(row);
            let a = span.start.max(row_start);
            let b = span.end.min(end);
            if a >= b {
                continue;
            }
            let start_x = self.doc.col(a);
            let end_x = if b < end {
                self.doc.col(b)
            } else if self.doc.text(b - 1..b) == "\r\n" {
                self.doc.col(b - 1)
            } else {
                self.doc.col(b)
            };
            let top = f64::from(r.top) + g.padding + visible as f64 * g.cell_height;
            let bottom = (top + g.cell_height).min(f64::from(r.bottom));
            for visual in self.doc.cell_maps[row].visual_ranges(start_x..end_x) {
                let left = f64::from(r.left) + g.padding + visual.start as f64 * g.cell_width;
                let right = (f64::from(r.left) + g.padding + visual.end as f64 * g.cell_width)
                    .min(f64::from(r.right));
                if right > left && bottom > top {
                    values.extend([left, top, right - left, bottom - top]);
                }
            }
        }
        let raw = array(VT_R8, values.len())?;
        for (i, v) in values.iter().enumerate() {
            put(raw, i, std::ptr::from_ref(v).cast())?;
        }
        Ok(raw)
    }
    fn GetEnclosingElement(&self) -> Result<IRawElementProviderSimple> {
        self.source.check()?;
        Ok(self.owner.clone())
    }
    fn GetText(&self, max: i32) -> Result<BSTR> {
        if max < -1 {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let text = self.doc.text(self.span()?);
        if max < 0 {
            return Ok(BSTR::from(text));
        }
        let mut used = 0;
        let cutoff = text
            .char_indices()
            .find_map(|(i, c)| {
                used += c.len_utf16();
                (used > max as usize).then_some(i)
            })
            .unwrap_or(text.len());
        Ok(BSTR::from(&text[..cutoff]))
    }
    fn Move(&self, unit: TextUnit, count: i32) -> Result<i32> {
        let r = self.span()?;
        if count == 0 {
            return Ok(0);
        }
        if r.is_empty() {
            let (p, n) = self.doc.advance(r.start, unit, count);
            self.assign(p..p)?;
            return Ok(n);
        }
        let normalized = self.doc.unit(r.start, unit);
        let (mut p, mut n) = self.doc.advance(normalized.start, unit, count);
        if p == self.doc.len() && n > 0 {
            p = self.doc.unit(p, unit).start;
            n -= 1;
        }
        self.assign(self.doc.unit(p, unit))?;
        Ok(n)
    }
    fn MoveEndpointByUnit(
        &self,
        e: TextPatternRangeEndpoint,
        unit: TextUnit,
        count: i32,
    ) -> Result<i32> {
        let mut r = self.span()?;
        let (p, n) = self.doc.advance(endpoint(&r, e)?, unit, count);
        set_endpoint(&mut r, e, p)?;
        self.assign(r)?;
        Ok(n)
    }
    fn MoveEndpointByRange(
        &self,
        e: TextPatternRangeEndpoint,
        target: Ref<ITextRangeProvider>,
        other: TextPatternRangeEndpoint,
    ) -> Result<()> {
        let mut r = self.span()?;
        set_endpoint(&mut r, e, endpoint(&self.other(target)?, other)?)?;
        self.assign(r)
    }
    fn Select(&self) -> Result<()> {
        self.source.select(&self.doc, self.span()?)
    }
    fn AddToSelection(&self) -> Result<()> {
        Err(unsupported())
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        Err(unsupported())
    }
    fn ScrollIntoView(&self, align_to_top: BOOL) -> Result<()> {
        self.source
            .scroll(&self.doc, self.span()?, align_to_top.as_bool())
    }
    fn GetChildren(&self) -> Result<*mut SAFEARRAY> {
        self.source.check()?;
        array(VT_UNKNOWN, 0)
    }
}
