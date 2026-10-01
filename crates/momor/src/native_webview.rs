//! Windows WebView2 host used by Momor's browser panel.
//!
//! The WebView2 controller is a child of the Momor window. This keeps the
//! browser content native and interactive while Momor owns the surrounding UI.

#![cfg(target_os = "windows")]

use anyhow::{Context as _, Result};
use std::{path::Path, sync::mpsc};
use webview2_com::{
    CoTaskMemPWSTR, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, Microsoft::Web::WebView2::Win32::*,
};
use windows_062::{
    Win32::{
        Foundation::{E_POINTER, HWND, RECT},
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
    },
    core::{BOOL, PCWSTR},
};

pub struct NativeWebView {
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
}

impl NativeWebView {
    pub fn new(parent: HWND, profile_dir: &Path) -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .context("falha ao inicializar COM para o WebView2")?;

        let profile = CoTaskMemPWSTR::from(profile_dir.to_string_lossy().as_ref());
        let environment = {
            let (tx, rx) = mpsc::channel();
            CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
                Box::new(move |handler| unsafe {
                    CreateCoreWebView2EnvironmentWithOptions(
                        PCWSTR::null(),
                        *profile.as_ref().as_pcwstr(),
                        None,
                        &handler,
                    )
                    .map_err(webview2_com::Error::WindowsError)
                }),
                Box::new(move |error_code, environment| {
                    error_code?;
                    tx.send(environment.ok_or_else(|| windows_062::core::Error::from(E_POINTER)))
                        .map_err(|_| windows_062::core::Error::from(E_POINTER))?;
                    Ok(())
                }),
            )
            .context("falha ao criar o ambiente WebView2")?;
            rx.recv()
                .map_err(|_| webview2_com::Error::SendError)
                .context("ambiente WebView2 não respondeu")??
        };

        let controller = {
            let (tx, rx) = mpsc::channel();
            CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
                Box::new(move |handler| unsafe {
                    environment
                        .CreateCoreWebView2Controller(parent, &handler)
                        .map_err(webview2_com::Error::WindowsError)
                }),
                Box::new(move |error_code, controller| {
                    error_code?;
                    tx.send(controller.ok_or_else(|| windows_062::core::Error::from(E_POINTER)))
                        .map_err(|_| windows_062::core::Error::from(E_POINTER))?;
                    Ok(())
                }),
            )
            .context("falha ao criar o controlador WebView2")?;
            rx.recv()
                .map_err(|_| webview2_com::Error::SendError)
                .context("controlador WebView2 não respondeu")??
        };

        let webview =
            unsafe { controller.CoreWebView2() }.context("WebView2 não retornou a página")?;
        unsafe {
            webview
                .Settings()?
                .SetAreDevToolsEnabled(true)
                .context("não foi possível habilitar o DevTools")?;
            controller
                .SetIsVisible(true)
                .context("não foi possível exibir o WebView2")?;
        }

        Ok(Self {
            controller,
            webview,
        })
    }

    pub fn set_bounds(&self, width: i32, height: i32) -> Result<()> {
        unsafe {
            self.controller
                .SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: width.max(1),
                    bottom: height.max(1),
                })
                .context("não foi possível redimensionar o WebView2")?;
        }
        Ok(())
    }

    pub fn navigate(&self, url: &str) -> Result<()> {
        let url = CoTaskMemPWSTR::from(url);
        unsafe {
            self.webview
                .Navigate(*url.as_ref().as_pcwstr())
                .context("não foi possível navegar no WebView2")?;
        }
        Ok(())
    }

    pub fn reload(&self) -> Result<()> {
        unsafe {
            self.webview
                .Reload()
                .context("não foi possível atualizar a página")?;
        }
        Ok(())
    }

    pub fn go_back(&self) -> Result<bool> {
        unsafe {
            let mut can_go_back = BOOL(0);
            self.webview
                .CanGoBack(&mut can_go_back)
                .context("falha ao consultar histórico")?;
            if !can_go_back.as_bool() {
                return Ok(false);
            }
            self.webview
                .GoBack()
                .context("não foi possível voltar no histórico")?;
        }
        Ok(true)
    }

    pub fn go_forward(&self) -> Result<bool> {
        unsafe {
            let mut can_go_forward = BOOL(0);
            self.webview
                .CanGoForward(&mut can_go_forward)
                .context("falha ao consultar histórico")?;
            if !can_go_forward.as_bool() {
                return Ok(false);
            }
            self.webview
                .GoForward()
                .context("não foi possível avançar no histórico")?;
        }
        Ok(true)
    }

    pub fn open_devtools(&self) -> Result<()> {
        unsafe {
            self.webview
                .OpenDevToolsWindow()
                .context("não foi possível abrir o DevTools")?;
        }
        Ok(())
    }
}

impl Drop for NativeWebView {
    fn drop(&mut self) {
        unsafe {
            let _ = self.controller.Close();
        }
    }
}
