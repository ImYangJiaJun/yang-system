import AppLayout from "@/layout/AppLayout";
import ModulePage from "@/pages/ModulePage";
import LoginPage from "@/pages/LoginPage";

import { RedirectIfAuthed, RequireAuth } from "./auth-gate";
import HomeRedirect from "./home-redirect";

export const appRoutes = [
  {
    path: "/login",
    element: (
      <RedirectIfAuthed>
        <LoginPage />
      </RedirectIfAuthed>
    ),
  },
  {
    path: "/",
    element: (
      <RequireAuth>
        <AppLayout />
      </RequireAuth>
    ),
    children: [
      { index: true, element: <HomeRedirect /> },
      { path: "m/:moduleId", element: <ModulePage /> },
      { path: "m/:moduleId/v/:viewId", element: <ModulePage /> },
    ],
  },
];
