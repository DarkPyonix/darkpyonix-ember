"""Starboard Notebook: Python support"""
import darkpyonix


# %% [code]
import torch
import torch.nn as nn
import torch.distributed as dist


# %% [argparse]
MODEL_ID = darkpyonix.params.get("model_id", default=0, choices=["default_model", "swin_t", "resnet", "vi-t"])  # model_id = ["default_model", "swin_t", "resnet", "vi-t"][0]  # python.exe run.py --model_id swin_t
MODEL_HEIGHT = darkpyonix.params.get("model_height", default=50, range=(30, 100, 1))  # model_height = 50  # python.exe run.py --model_height 50
BASE_LEARNING_RATE = darkpyonix.params.get("base_learning_rate", default=0, choices=[1e-5, 1e-3])  # base_learning_rate = 0.001  # python.exe run.py --base_learning_rate 0.001


# %% [code]
print("Running preprocessor for Python support...")


# %% [markdown]
darkpyonix.markdown("""
# 🐍 Python support in Starboard Notebook
Python support is built on top of [Pyodide](https://hacks.mozilla.org/2019/04/pyodide-bringing-the-scientific-python-stack-to-the-browser/), a WebAssembly powered Python runtime in the browser that supports most of the common scientific packages such as Numpy, Pandas and Matplotlib.

Using Starboard you can create a Python notebook without any backend server.

> New to Starboard? Check out the [Starboard introduction notebook](https://starboard.gg/#introduction) for an overview of Starboard itself.
""", silent=True)


# %% [code]
# When you first run this cell it will load the Python runtime.
# This runtime is a few megabytes in size. Your browser should cache it, so the next time it should load faster.
message = "Hello Python!"
print(message)

x = [i**2 for i in range(5)]
x


# %% [binding]
@darkpyonix.binding
class Classifier(nn.Module):
    def __init__(self, backbone: str):
        super().__init__()
        import timm
        self.net = timm.create_model(backbone)

    def forward(self, x):
        return self.net(x)


# %% [binding]
@darkpyonix.binding
def train_step(model, batch, optimizer):
    optimizer.zero_grad()
    x, y = batch
    loss = nn.functional.cross_entropy(model(x), y)
    loss.backward()
    optimizer.step()
    return loss.item()


# %% [shell]
darkpyonix.run_command("""nvidia-smi --query-gpu=name,memory.total --format=csv""")


# %% [markdown]
darkpyonix.markdown("""
## Visualizing car data using pandas and matplotlib

Analyzing, explaining and visualizing data is a great usecase for Starboard notebooks.

In this example we'll load datasets from [Selva Prabhakaran's ML dataset repository on Github](https://github.com/selva86/datasets) and visualize them. The code below is based on examples from their matplotlib examples [blog post](https://www.machinelearningplus.com/plots/top-50-matplotlib-visualizations-the-master-plots-python/).

> Matplotlib plots are not really made for mobile devices, interactivity can be a bit buggy and they may overflow.

> Consider viewing below examples on a desktop.
""", slient=False)


# %% [code]
# You can import many of the common scientific Python packages such as numpy or pandas
# A full list can be found here https://github.com/iodide-project/pyodide/tree/master/packages
# Packages are downloaded and installed dynamically, they are cached afterwards.
import pandas as pd
import matplotlib.pyplot as plt
import js

url = "https://raw.githubusercontent.com/selva86/datasets/master/mtcars.csv"

# Prepare Data
df = pd.read_csv(js.open_url(url))

x = df.loc[:, ['mpg']]
df['mpg_z'] = (x - x.mean())/x.std()
df['colors'] = ['red' if x < 0 else 'green' for x in df['mpg_z']]
df.sort_values('mpg_z', inplace=True)
df.reset_index(inplace=True)

df


# %% [code]
# Draw plot
plt.figure(figsize=(10,8), dpi= 80)
plt.hlines(y=df.index, xmin=0, xmax=df.mpg_z, color=df.colors, alpha=0.4, linewidth=5)

# Decorations
plt.gca().set(ylabel='$Model$', xlabel='$Mileage$ (standard deviations)')
plt.yticks(df.index, df.cars, fontsize=8)
plt.title('Diverging Bars of Car Mileage', fontdict={'size':16})
plt.grid(linestyle='--', alpha=0.5)
plt.show()


# %% [code]
# By using micropip we can install additional packages that don't ship with Pyodide by default.
# Most pure python package work in the browser, here we install "squarify"
import micropip
micropip.install("squarify")

darkpyonix.uv.add("transformers")
darkpyonix.uv.remove("datasets")

darkpyonix.pip.install("requests")  # darkpyonix.uv.pip.install


# %% [code]
import squarify 

url = "https://raw.githubusercontent.com/selva86/datasets/master/mpg_ggplot2.csv"

df = pd.read_csv(js.open_url(url))
df = df.groupby('class').size().reset_index(name='counts')
labels = df.apply(lambda x: str(x[0]) + "\n (" + str(x[1]) + ")", axis=1)
sizes = df['counts'].values.tolist()
colors = [plt.cm.Spectral(i/float(len(labels))) for i in range(len(labels))]

# Draw Plot
plt.figure(figsize=(8,5), dpi= 80)
squarify.plot(sizes=sizes, label=labels, color=colors, alpha=.8)

plt.title('Treemap of Vehicle Class')
plt.axis('off')
plt.show()


# %% [code]
# You can run java, kotlin, swift code in your python interpreter
# if you are using python-multiplatform.
import kotlin
import java
import swift

from kotlinx.corroutines import Launch
Launch.run()


# %% [cinterop]
darkpyonix.run_cinterop("""
    blabla
""")


# %% [cppinterop]
darkpyonix.run_cppinterop("""
    blabla
""")


# %% [rustinterop]
darkpyonix.run_rustinterop("""
    blabla
""")


# %% [parallel]
__co_routines__ = []


# %% [code]  
# @width: 1fr
__co_routines__.append(data.load("imagenet", split="train"))


# %% [code] 
# @width: 1fr
__co_routines__.append(data.load("imagenet", split="val"))


# %% [concorrunt]
if __name__  == '__main__':
    darkpyonix.run_parallel(*__co_routines__)  # automatically await corrutines


# %% [code] 
def to_tensor(raw):
    pass

converted = to_tensor(torch.from_numpy())
