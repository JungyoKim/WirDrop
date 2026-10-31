//! UI Automation으로 화면 읽기 프로그램에 보이는 섬. 창은 조각 루트 하나이고, 그 이름은 카드의
//! 글이며, 자식은 카드의 버튼들(Invoke는 클릭처럼 누릅니다)과 입력칸(편집 상자; 암호 칸은 입력한
//! 것을 알려 주지 않습니다)입니다. 새 카드는 알림처럼 소리 내어 알립니다 (내레이터가 읽습니다).
//!
//! UI Automation은 창이 바쁠 수 있는 동안 자기 스레드에서 묻기 때문에, 떠 있는 것의 사본([State])을
//! 읽고 누르기는 창에 메시지([WM_PRESS])로 보냅니다.
// UI Automation의 상수들, 이름 그대로.
#![allow(non_upper_case_globals)]
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, PostMessageW, WM_APP};
use windows::core::{BOOL, BSTR, Error, IUnknown, IUnknownImpl, Interface, PCWSTR, Result, implement};

/// 화면 읽기 프로그램의 누르기: wparam은 카드 버튼 가운데 그 번호.
pub const WM_PRESS: u32 = WM_APP + 4;
/// UI Automation이 더는 그 요소에 닿을 수 없습니다 (카드가 바뀌었습니다).
const ELEMENT_NOT_AVAILABLE: windows::core::HRESULT = windows::core::HRESULT(0x8004_0201_u32 as i32);

/// UI Automation에 보일 떠 있는 것.
#[derive(Default)]
pub struct State {
    hwnd: isize,
    /// 한 덩어리로 읽는 카드의 글.
    name: String,
    elements: Vec<Element>,
    /// 카드가 바뀔 때 올립니다: 그 전의 요소는 사라진 것입니다.
    version: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Role {
    /// 카드 버튼 가운데 번호.
    Button(usize),
    /// 입력칸: 암호 칸인지, 키보드가 들어 있는지.
    Field { secret: bool, focused: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub role: Role,
    pub name: String,
    /// 화면 위 위치(픽셀).
    pub rect: RECT,
}

/// 창의 접근성: UI Automation이 읽는 상태와 그 루트 요소.
pub struct Access {
    state: Arc<Mutex<State>>,
    root: IRawElementProviderSimple,
}

impl Access {
    pub fn new(hwnd: HWND) -> Access {
        let state = Arc::new(Mutex::new(State {
            hwnd: hwnd.0 as isize,
            ..State::default()
        }));
        let root: IRawElementProviderSimple = Root { state: state.clone() }.into();
        Access { state, root }
    }

    /// WM_GETOBJECT: UI Automation이 물으면 루트 요소.
    pub fn get_object(&self, hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        // SAFETY: UI Automation이 청하는 대로 넘기는 창 자신의 공급자.
        (lparam.0 as i32 == UiaRootObjectId).then(|| unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, &self.root) })
    }

    /// 지금 떠 있는 것. 바뀐 것이 없으면 아무것도 하지 않습니다.
    pub fn set(&self, name: String, elements: Vec<Element>) {
        let mut s = self.state.lock().unwrap();
        if s.name != name || s.elements != elements {
            // 입력칸의 포커스만 바뀌었습니다: 요소는 그대로.
            let same = s.elements.len() == elements.len() && s.elements.iter().zip(&elements).all(|(a, b)| a.name == b.name && a.rect == b.rect);
            if !same {
                s.version = s.version.wrapping_add(1);
            }
            s.name = name;
            s.elements = elements;
        }
    }

    /// 화면 읽기 프로그램이 text를 지금 말하게 합니다 (새 카드).
    pub fn announce(&self, text: &str) {
        // SAFETY: 우리 공급자와, 호출보다 오래 사는 문자열을 쓰는 평범한 호출.
        unsafe {
            if !UiaClientsAreListening().as_bool() {
                return;
            }
            tracing::debug!("island: telling screen readers {text:?}");
            let _ = UiaRaiseNotificationEvent(
                &self.root,
                NotificationKind_ItemAdded,
                NotificationProcessing_ImportantMostRecent,
                &BSTR::from(text),
                &BSTR::from("WirDrop.Island"),
            );
        }
    }
}

fn hwnd(state: &Mutex<State>) -> HWND {
    HWND(state.lock().unwrap().hwnd as _)
}

fn uia_rect(r: &RECT) -> UiaRect {
    UiaRect {
        left: r.left.into(),
        top: r.top.into(),
        width: (r.right - r.left).into(),
        height: (r.bottom - r.top).into(),
    }
}

fn none<T>() -> Result<T> {
    Err(Error::empty())
}

// ---- 창

#[implement(IRawElementProviderSimple, IRawElementProviderFragment, IRawElementProviderFragmentRoot)]
struct Root {
    state: Arc<Mutex<State>>,
}

impl Root_Impl {
    fn child(&self, index: usize) -> Result<IRawElementProviderFragment> {
        let s = self.state.lock().unwrap();
        if index >= s.elements.len() {
            return none();
        }
        let root: IRawElementProviderFragmentRoot = self.to_interface();
        Ok(Child {
            state: self.state.clone(),
            root,
            index,
            version: s.version,
        }
        .into())
    }
}

impl IRawElementProviderSimple_Impl for Root_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, _: UIA_PATTERN_ID) -> Result<IUnknown> {
        none()
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        Ok(match id {
            UIA_NamePropertyId => VARIANT::from(BSTR::from(self.state.lock().unwrap().name.as_str())),
            UIA_ControlTypePropertyId => VARIANT::from(UIA_PaneControlTypeId.0),
            UIA_LiveSettingPropertyId => VARIANT::from(Assertive.0),
            UIA_IsKeyboardFocusablePropertyId => VARIANT::from(false),
            _ => VARIANT::default(),
        })
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        // SAFETY: 우리 창.
        unsafe { UiaHostProviderFromHwnd(hwnd(&self.state)) }
    }
}

impl IRawElementProviderFragment_Impl for Root_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let n = self.state.lock().unwrap().elements.len();
        match direction {
            NavigateDirection_FirstChild if n > 0 => self.child(0),
            NavigateDirection_LastChild if n > 0 => self.child(n - 1),
            _ => none(),
        }
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        // 창 루트라 호스트의 것.
        Ok(std::ptr::null_mut())
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let mut r = RECT::default();
        // SAFETY: 우리 창의 위치를 지역 변수로.
        unsafe { GetWindowRect(hwnd(&self.state), &mut r)? };
        Ok(uia_rect(&r))
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }
    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.to_interface())
    }
}

impl IRawElementProviderFragmentRoot_Impl for Root_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let at = POINT { x: x as i32, y: y as i32 };
        let found = self.state.lock().unwrap().elements.iter().position(|e| {
            let r = e.rect;
            at.x >= r.left && at.x < r.right && at.y >= r.top && at.y < r.bottom
        });
        match found {
            Some(i) => self.child(i),
            None => Ok(self.to_interface()),
        }
    }
    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        let found = self
            .state
            .lock()
            .unwrap()
            .elements
            .iter()
            .position(|e| matches!(e.role, Role::Field { focused: true, .. }));
        match found {
            Some(i) => self.child(i),
            None => none(),
        }
    }
}

// ---- 버튼이나 입력칸

#[implement(IRawElementProviderSimple, IRawElementProviderFragment, IInvokeProvider, IValueProvider)]
struct Child {
    state: Arc<Mutex<State>>,
    root: IRawElementProviderFragmentRoot,
    index: usize,
    version: u32,
}

impl Child_Impl {
    /// 요소의 카드가 아직 떠 있으면 그 요소.
    fn element(&self) -> Result<Element> {
        let s = self.state.lock().unwrap();
        match s.elements.get(self.index) {
            Some(e) if s.version == self.version => Ok(e.clone()),
            _ => Err(ELEMENT_NOT_AVAILABLE.into()),
        }
    }

    fn sibling(&self, index: usize) -> Result<IRawElementProviderFragment> {
        let s = self.state.lock().unwrap();
        if s.version != self.version || index >= s.elements.len() {
            return none();
        }
        Ok(Child {
            state: self.state.clone(),
            root: self.root.clone(),
            index,
            version: self.version,
        }
        .into())
    }
}

impl IRawElementProviderSimple_Impl for Child_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        let e = self.element()?;
        match (pattern, &e.role) {
            (UIA_InvokePatternId, Role::Button(_)) => self.to_interface::<IInvokeProvider>().cast(),
            (UIA_ValuePatternId, Role::Field { .. }) => self.to_interface::<IValueProvider>().cast(),
            _ => none(),
        }
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let e = self.element()?;
        let field = matches!(e.role, Role::Field { .. });
        Ok(match id {
            UIA_NamePropertyId => VARIANT::from(BSTR::from(e.name.as_str())),
            UIA_ControlTypePropertyId => VARIANT::from(if field { UIA_EditControlTypeId.0 } else { UIA_ButtonControlTypeId.0 }),
            UIA_IsPasswordPropertyId => VARIANT::from(matches!(e.role, Role::Field { secret: true, .. })),
            UIA_IsKeyboardFocusablePropertyId => VARIANT::from(field),
            UIA_HasKeyboardFocusPropertyId => VARIANT::from(matches!(e.role, Role::Field { focused: true, .. })),
            UIA_IsEnabledPropertyId => VARIANT::from(true),
            _ => VARIANT::default(),
        })
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        none()
    }
}

impl IRawElementProviderFragment_Impl for Child_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        match direction {
            NavigateDirection_Parent => self.root.cast(),
            NavigateDirection_NextSibling => self.sibling(self.index + 1),
            NavigateDirection_PreviousSibling if self.index > 0 => self.sibling(self.index - 1),
            _ => none(),
        }
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        // SAFETY: 새 i32 두 개짜리 배열을 그 자리에서 채웁니다.
        unsafe {
            let ids = SafeArrayCreateVector(VT_I4, 0, 2);
            if ids.is_null() {
                return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
            }
            let values = [UiaAppendRuntimeId as i32, (self.version as i32) << 8 | self.index as i32];
            for (i, v) in values.iter().enumerate() {
                SafeArrayPutElement(ids, &(i as i32), (v as *const i32).cast())?;
            }
            Ok(ids)
        }
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(uia_rect(&self.element()?.rect))
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }
    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.root.clone())
    }
}

impl IInvokeProvider_Impl for Child_Impl {
    fn Invoke(&self) -> Result<()> {
        let Role::Button(index) = self.element()?.role else {
            return Err(ELEMENT_NOT_AVAILABLE.into());
        };
        // SAFETY: 우리 창에 보내는 메시지.
        unsafe { PostMessageW(Some(hwnd(&self.state)), WM_PRESS, WPARAM(index), LPARAM(0)) }
    }
}

impl IValueProvider_Impl for Child_Impl {
    fn SetValue(&self, _: &PCWSTR) -> Result<()> {
        // 값을 설정하는 것이 아니라 키보드로 입력합니다.
        Err(windows::Win32::Foundation::E_ACCESSDENIED.into())
    }
    fn Value(&self) -> Result<BSTR> {
        // 입력한 것은 알려 주지 않습니다 (암호, Apple ID).
        Ok(BSTR::new())
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(BOOL(0))
    }
}
