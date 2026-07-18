from manim import *
from manim.mobject.opengl.opengl_surface import OpenGLSurface
from manim.renderer.shader import Mesh


class MySurface(OpenGLSurface):
    pass


class Bad(Scene):
    def construct(self):
        # Direct add of an OpenGLMobject-rooted surface mesh.
        surface = OpenGLSurface(lambda u, v: (u, v, 0))
        self.add(surface)
        # Direct add of a shader-attribute mesh (Object3D scene object).
        mesh = Mesh()
        self.add(mesh)
        # A project subclass of a curated mesh is a mesh too.
        self.add(MySurface(lambda u, v: (u, v, 0)))
        # Introducer setup-add: the play adds the mesh to the scene.
        self.play(FadeIn(OpenGLSurface(lambda u, v: (u, v, 0))))
        self.wait()
